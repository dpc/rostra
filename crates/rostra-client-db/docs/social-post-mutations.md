# Local own-text mutation requests

`Client::mutate_social_post_idempotent` accepts a caller-persisted random
`SocialPostMutationRequestId` and immutable `SocialPostMutationRequest`.
`expected_current` is the **full** event ID, not a short prefix. `Edit { body }`
preserves exact UTF-8 bytes; empty/whitespace text is rejected rather than
interpreted as deletion. `Delete` explicitly emits a blank text payload with the
existing delete-auxiliary-parent flag. Reactions are a separate operation.

## Deliberately narrow target scope

Only owned SocialPost text/replies with locally available supported payloads can
be mutated. News, URL/title fields, reactions, singleton events, unknown fields,
duplicate fields, trailing CBOR data, non-map forms and invalid typed values are
rejected, not silently lost. The strict decoder preserves the original legacy
persona, declared persona tags and reply reference independently; it does not
replace legacy fields with their effective fallback. It accepts omitted optional
fields and indefinite maps. The pinned decoder rejects chunked strings.
Wire byte order and encoding are not
preserved; the known fields' typed meanings are. Remote ingestion is unchanged.

Supported target bytes and encoded candidate bytes are limited to 1,000,000
bytes. The edit body is also checked against this bound before encoding.
`prepare_social_post_mutation(full_id)` returns the original `SocialPost` after
the same target checks, from a read-only snapshot. Hosts can use it to expose
safe edit/delete affordances without duplicating the supported-type decoder.
It does not reserve the target.

Full-ID equality is checked against the retained signed envelope before checking
ownership and kind. Canonical validation walks retained reverse lineage to its
root, then follows the existing lowest-immediate-short-ID replacement ordering,
not timestamps. This also rejects a noncanonical fork's otherwise current leaf.
Each direction allows at most 1024 edges; longer/cyclic traversal fails with
`SocialPostMutationLineageLimit`, without modifying anything. No traversal
silently truncates.

## Atomic acceptance and reconciliation

The authoritative transaction checks the mutation receipt first, then validates
input and the expected current target, verifies the candidate's supported
payload matches the requested body plus preserved metadata, and ingests the
candidate and receipt. The existing admission machinery runs inside this same
serialized write transaction. Failed admission or storage rolls back the
header, target deletion, replacement indexes and receipt together.

Concurrent edits/deletes of one expected target cannot both be accepted locally.
Same-ID calls may sign different candidates, but the original committed receipt
wins; losing candidates are not ingested. A successful retry returns that
original signed event **before** consulting target state. It does not require
original/current payload bytes, reingest the event, or resurrect deleted/pruned
content. Receipt mismatch is distinct from stale-target conflict.

This is a local snapshot/transaction guarantee. Remote same-author events arriving
after commit may change lineage or delete the result. No cross-device CAS,
delivery guarantee, timestamp workaround or new wire protocol is introduced.

Preflight preparation rechecks receipts on failure to reconcile a concurrent
commit between the initial receipt read and preparation. A validation rejection
means that call accepted no candidate; it does not promise another independently
in-flight caller cannot later commit. Hosts should serialize pending operations
before clearing their persisted inputs. Cancellation/shutdown/commit-before-ack
semantics are the same as [text publication](social-post-idempotency.md): a
started synchronous transaction may commit after its waiter is dropped.
Closing clients reject new calls, including receipt-only retries.

## Deletion classification and lineage

Existing reducers index **nonblank edits**, not blank delete events, as
replacement lineage. After deletion the original/latest target therefore remains
the canonical event with `EventContentAvailability::Deleted`; the delete event
is not a new canonical post version. Retained replacement rows preserve original
and reply lineage independently of payload deletion.

`SocialPostState::is_tombstone` is separate: it is true only for an available
SocialPost event with an auxiliary parent, deletion flag and a strictly supported
blank text payload. Missing projection plus a replacement flag is not proof of
deletion. Unsupported or unavailable payloads never set this field. Deleting or
pruning the tombstone's own payload clears this available-payload classification,
without undoing the target's durable Deleted state.

## Namespace, digest and retention

Schema **35** creates `social_post_mutation_receipts`, independent of create
receipts even when IDs coincide. Older binaries reject schema 35. There is no
backfill. Total migration preserves both receipt tables byte-for-byte in place,
including stash/reopen recovery, rather than reconstructing them from events.

V1 input identity uses BLAKE3 derive-key context
`rostra.social_post_mutation.input.v1`, followed by:

1. Version byte `01`.
2. Raw full account ID (32 bytes).
3. Raw full expected-current event ID (32 bytes).
4. Operation byte: `00` for Edit, `01` for Delete.
5. Only for Edit: UTF-8 length as u64 big-endian, then exact body bytes.

The request ID is the table key, not digest input. Fields/encoding and digest
domain must remain stable for V1. Equality assumes BLAKE3 collision and
second-preimage resistance.

Receipts retain only a versioned digest and original signed event indefinitely,
with no expiration/individual erase operation. Fixed-size rows are outside
payload quotas; total row count is unbounded. They contain no reversible body
copy, but expose signed metadata and allow offline guesses of low-entropy
inputs. They are not encryption or forensic erasure. Whole-account deletion,
loss, rollback, or independently restored DBs end the guarantee. Storage-engine
durability assumptions apply; tests do not promise arbitrary OS-kill/power-loss
recovery.
