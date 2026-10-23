# Durable independent reactions and exact bounded summaries

`Client::react_idempotent` accepts a caller-persisted random
`ReactionRequestId([u8; 16])` and immutable `ReactionRequest`. The namespace is
account-local and separate from text create/edit/delete requests. `Add` creates
another independent reaction, even when the same author already reacted.
`Remove { expected_current }` removes precisely that full owned event ID, not an
arbitrarily selected row, singleton or followed replacement head.

`Change` is reserved but unsupported and never creates a receipt. Existing
reducers treat deleting-aux e-only payloads as deletion without replacement
projection. Reinterpreting them would materialize previously skipped historical
events on replay, while older clients would still see deletion only. This API
does not change those semantics or add an intent marker. A future Change API
requires a separate compatibility decision, not just accepting this variant.

## Validation and local acceptance

The target tuple binds a **full** author and EventId to its retained signed
envelope before shortening for the wire. Add requires the canonical current
available supported nonblank text/reply target; news, reactions, singleton
events, unknown fields and unsupported CBOR are rejected. Remove accepts a
retained target on its canonical history even after that target's payload was
deleted or pruned. The selected reaction must remain available, self-owned and
canonical/current, with its exact reply author and version belonging to the
target's canonical history. Remove does not need target payload bytes and
therefore does not reclassify their original text/news shape.

Supported reaction bytes are strictly typed CBOR maps, at most 1,000,000 bytes:
only known fields, no duplicates, trailing data, invalid field types, news,
URL/title metadata or mixed text/reaction payloads. Optional legacy persona and
declared persona tags decode independently without lossy fallback conversion.
Add emits a dedicated `e` payload; Remove emits a blank text tombstone without
incidental reply/reaction metadata. Existing ingestion and projection reducers
are unchanged.

Reaction strings use the **exact legacy compatibility heuristic**, not Unicode
emoji validation: at most eight UTF-8 bytes and the existing inference must
return the entire untrimmed input. Empty/ASCII text, whitespace normalization,
multiple graphemes and inferred prefixes are rejected. Non-ASCII letters may
still pass the legacy heuristic. Hosts may offer a narrower choice set.

`Database::prepare_reaction` is a read-only policy preflight, useful for advisory
affordances; it reserves nothing. The authoritative write transaction looks up
the receipt first, validates the request/target/selected row, independently checks
the candidate's exact supported payload and deletion parent, then admits,
ingests and receipts it atomically. Admission/storage failure rolls back header,
deletion, projection and receipt together. Different input under a committed key
returns `ReactionMismatch`. An unsupported request under a previously committed
different input also mismatches; it does not replace the receipt.

Concurrent same-key candidates may both be signed but only the winning event is
ingested. Distinct keys use distinct request-derived nonces in the existing
nonsingleton header `key_aux`, so equal head/timestamp/payload cannot silently
collapse independent adds. The singleton flag stays clear; unchanged old
SocialPost reducers ignore that auxiliary key. Candidate admission binds this
nonce and rejects an already-retained candidate unless a matching receipt already
reconciled it. Distinct removals of the same expected row cannot both commit locally.
Successful retries return the original signed event before consulting current
target/payload state, including after deletion, pruning or missing target data;
they do not reingest or resurrect content. Preflight failures recheck receipts
to reconcile concurrent commits. A validation error means this call did not
accept a candidate; another independently running caller may still commit.

This guarantees local acceptance, not remote delivery or cross-device CAS.
Later remote events can delete/change the accepted state. Cancellation,
commit-before-ack, shutdown and durability limitations match
[text requests](social-post-idempotency.md). A synchronous transaction may
commit after its async waiter is dropped; closing clients reject new calls,
including receipt-only retries. Hosts must persist immutable pending inputs and
serialize unresolved intents rather than signing newer intent after uncertainty.

## Exact materialized snapshot, independently paginated rows

`Database::reaction_summary((author, full_event), cursor, limit)` resolves the
canonical target history from any retained canonical version. In one read
snapshot it scans reaction indexes for **all versions on that history**, then
returns exact `total` and `self_count` of supported materialized rows plus an
independent page. Each independent reaction counts, including several from one
author. These are neither globally-known network counts nor unique-author votes.
They do not copy potentially stale aggregate counters.

Missing/deleted/pruned/unsupported payloads, noncanonical rows, mismatched
reply authors/versions, mismatched index timestamps and duplicate event IDs do
not contribute. Missing/unsupported target payload does not prevent inspecting
its retained history. `raw_rows_scanned` reports inspected index entries,
including rejected rows; it is not a reaction count. The supported-payload
filter deliberately excludes historical metadata this interface cannot safely
mutate, even if permissive remote ingestion indexed it.

Rows contain full retained author/event identities, signed timestamp and exact
reaction. Cursors order descending by `(timestamp, full EventId)` and are
exclusive. Counts do not shrink with page/cursor limits. `next_cursor` is present
only if more eligible rows exist in that snapshot. Each page call has its own
snapshot: intervening mutations can move membership across pages.

Hard bounds return typed errors **without partial counts or rows**:

* 1024 edges in each backward/forward traversal, and 4096 edges total per call:
  `ReactionHistoryLimit`.
* 10000 raw reaction-index entries across all target versions:
  `ReactionScanLimit`.
* 1,000,000 bytes per inspected payload and 16 MiB cumulative encoded bytes
  examined: `ReactionPayloadLimit`. Repeated inspection counts again.
* Page size must be 1..=100: `ReactionPageLimit`.

These bound traversal, decoding input and result memory, not a CPU deadline or
process RSS. Limit failures mean unavailable, not zero. A full count is computed
even for a small page; hosts should bound concurrent work.

## Schema, digest and retention

Schema **36** introduces empty `social_reaction_receipts`, with no backfill. Older
binaries reject the schema. Incremental upgrade leaves projections unchanged;
total migration preserves receipts byte-for-byte in place, including stash/reopen
recovery, rather than deriving them from event payloads.
This is receipt preservation, not a new retention guarantee: ordinary total
replay can rematerialize non-quota-pruned bytes still retained in the content
store. Receipt lookup itself never performs that replay or changes availability.

V1 uses BLAKE3 derive-key context `rostra.reaction_request.input.v1` over:

1. Version byte `01`, full account ID (32 bytes).
2. Full target author (32 bytes), then full target EventId (32 bytes).
3. Operation byte: `00` Add, `01` Remove, `02` unsupported Change.
4. For Remove/Change, full expected-current reaction ID (32 bytes).
5. For Add/Change, UTF-8 length as u64 big-endian followed by exact reaction bytes.

Request ID is the table key, not digest input. V1 encoding/domain must remain
stable. Each successful request retains only the digest and original signed
event indefinitely. There is no reversible input copy, expiration or individual
erase/reset API. Fixed-size rows remain outside payload quotas; total row count
is unbounded. Equality assumes BLAKE3 collision/second-preimage resistance.
Digests allow offline low-entropy input guesses, and signed metadata remains
visible: this is not encryption or forensic erasure. Whole-account deletion,
loss, rollback or independent restoration ends the guarantee.

The stable candidate nonce uses BLAKE3 derive-key context
`rostra.reaction_request.event_nonce.v1` over raw full account ID (32 bytes)
followed by raw request ID (16 bytes), taking the first 16 digest bytes as
`key_aux`. It is not an input-digest field, singleton key or auxiliary parent.
The signed header reveals this pseudorandom request-derived nonce; it does not
reveal the raw local request key. Account/key uniqueness relies on the same
cryptographic assumptions as event identity. Receipt input binding prevents key
reuse across operations, and existing-candidate rejection fails safely on an
event identity collision instead of collapsing two accepted intents.
