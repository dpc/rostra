# Durable shoutbox intents and bounded received traversal

These opt-in native APIs use the existing SHOUTBOX event format and reducers.
They do not alter web behavior or signed-event compatibility. Shoutbox remains
all-author, unfiltered, without personas, replies, or reactions.

## Immutable requests and local acceptance

`Client::post_shoutbox_idempotent(secret, ShoutboxRequestId, &ShoutboxRequest)`
accepts `{ body: String }`. The host trims the body **before** persisting a random
128-bit request ID and immutable input. Core requires already-trimmed text
containing 1..=1000 **UTF-8 bytes**, not characters, and never normalizes a retry.
Interior whitespace is preserved. Rust strings cannot contain invalid UTF-8.

Shoutbox keys occupy a separate account-local namespace. Receipt reads and the
authoritative write transaction reconcile the exact input digest before input
validation, payload admission, or mutable graph/content checks. Same-key retries
return the original signed event, including after prune/delete/rebuild, without
reingesting its payload. Different input returns `ShoutboxRequestMismatch`, even
if the changed input is invalid. New input/candidate failures return
`ShoutboxInvalidInput`/`ShoutboxCandidate`. Existing signing-account and shutdown
checks apply before receipt access; shutdown rejects new calls including retries.

Candidate validation, admission, graph changes, content reduction, reception
allocation and permanent receipt insertion share one serialized transaction.
Precommit failure rolls everything back, including the allocation counter.
Existing publication hooks run after commit. Distinct keys with identical
head/time/body use a domain-separated nonsingleton auxiliary nonce. SHOUTBOX is
not a singleton; the ordinary reducer ignores that auxiliary key. Admission also
rejects a previously retained candidate lacking the request's receipt.

Success means **local acceptance**, not remote delivery or cross-device
deduplication. Storage failure, cancellation, post-commit hook panic, process
death, or shutdown racing a started transaction may leave an uncertain outcome.
Dropping the waiter does not cancel an executing synchronous transaction. Persist
and retry the same key/input; do not issue a new key to recover uncertainty.
Rejections do not prevent a separately running same-key call from committing.
Client errors reconcile a concurrently accepted receipt before returning failure.

## Bounded traversal

`Database::paginate_shoutbox(Option<ShoutboxCursor>, ShoutboxBounds)` is fallible.
Bounds require `rows` in 1..=100, `raw_rows` in 1..=4096, and `payload_bytes` in
1..=16 MiB. Invalid bounds return `ShoutboxBounds`. There is no total traversal
cap. These bound input/work/output, not a CPU deadline or process RSS guarantee.
Hosts must bound concurrent work and run it away from their UI executor.

The first call captures the durable **next unused allocation sequence**, then
scans in the same read transaction. Continuations exclude `seq >= next_sequence`,
including newly allocated rows with older received timestamps and delayed
materialization. The allocation bound is exclusive, unlike an inclusive latest
row position. Raw scanning counts these excluded entries. All-author order is
descending `(effective received timestamp, sequence)`, not signed author time.
Sequence tie-breaking prevents same-timestamp collisions and omissions.

The opaque serde cursor binds full account, schema38's durable reception-index
epoch, a per-open random incarnation, the captured allocation bound, and an
exclusive **last consumed raw position**. Reopen or total rebuild returns
`ShoutboxCursorStale`, never an automatic restart. Reusing the per-open database
incarnation with the rebuild epoch prevents sequence-reset ABA, including reopening
an older backup. Random incarnation uniqueness assumes 128-bit collision
resistance. Tokens are trusted local transport state, not authenticated input or
authorization. They are deliberately not durable chat snapshots.

This is **not** a frozen cross-page membership/availability snapshot. Pruning,
deletion or restoration can change later page contents. Restoration/replay that
allocates a new sequence is excluded until explicit refresh. No separate
new-arrival/seen/unread API is included: use existing foreground invalidation to
offer Refresh. A host may retain its loaded rows on stale, then explicitly reset
the window when refreshing.

Rows contain full signed `(RostraId, EventId)` identity, the full received
position, signed timestamp, signed `encoded_bytes`, and either exact Shoutbox
content or an explicit `ShoutboxIssue` placeholder. Oversized means the payload
exceeds the caller's **entire** byte budget or the native 1,000,000-byte policy;
it is consumed without owned payload decode. Unsupported means outside the
bounded known flat CBOR-map policy (unknown/duplicate fields, non-string body,
non-map forms or trailing bytes). Historical body whitespace is preserved, and
reads do not impose current composer validation. Neither issue declares signed
protocol content invalid. Hosts must visibly disclose placeholders rather than
silently hiding them. Placeholders count toward the output-row cap.

The reader checks borrowed stored lengths before decoding, binds full IDs to
envelopes, and checks hashes for payloads it decodes. It does no lineage traversal,
social filtering, whole-database collection or eager generic CBOR value parsing.
Absent/deleted/pruned payloads count as `unavailable_rows`. Malformed index,
envelope binding, stored length/hash, or storage decoding returns an error without
partial results. Oversized payload contents are not hashed or validated.

`ShoutboxStop` distinguishes End, PageLimit, ScanBudget and PayloadBudget.
`raw_rows_scanned` counts consumed entries; the iterator may make one additional
unconsumed boundary probe. `payload_bytes_read` counts encoded bytes handed to the
bounded decoder. A cumulative byte boundary leaves that row unconsumed; it fits
a fresh budget. An individually oversized row instead becomes a consumed
placeholder, so a tiny budget never creates a nonadvancing continuation.
Every non-End page advances and includes `next_cursor`, even if empty. Continue
means more raw scanning, not a promise of another available row. Only End/no
cursor proves exhaustion in that read transaction.

## Schema, permanence and privacy

Schema **39** creates empty `shoutbox_request_receipts`; no historical receipts
are inferred. Incremental upgrade preserves existing reception allocations and
epoch. Total rebuild preserves receipts byte-for-byte in place, including stash
interruption/reopen, while rebuilding indexes and replacing their epoch.
The existing reserved `shoutbox_` prefix protects this table from extension
transactions. Older binaries reject schema39; downgrade requires an explicit
compatible storage strategy, not merely changing the schema number.

Receipt V1 stores an input digest and signed header, not a reversible body copy.
Digest derivation uses BLAKE3 context `rostra.shoutbox_request.input.v1`, byte
`01`, raw account ID (32 bytes), then body UTF-8 byte length as u64 big endian and
exact body bytes. Request ID is the table key, not digest input.
The nonce uses `rostra.shoutbox_request.event_nonce.v1`, raw account ID followed
by request ID (16 bytes), taking the first 16 digest bytes. These V1 domains and
encodings must remain stable.

Receipt rows are fixed-size metadata outside payload quotas; their total count
grows without bound. They never expire and have no individual reset/erase API.
Digests enable offline guesses of low-entropy text, and signed metadata remains
visible. This is not encryption or forensic erasure. Equality assumes BLAKE3
collision/second-preimage resistance. Whole-account deletion, loss, rollback or
independent restore ends the receipt guarantee. Ordinary replay can rematerialize
retained non-quota-pruned content independently of receipt lookup.
