# Local text-post idempotency

`Client::social_post_idempotent` publishes text, not heuristic reactions. Its
`SocialPostRequestId` is a caller-generated random 128-bit value scoped to this
operation in one identity-bound database. Persist the ID and immutable
`SocialPostRequest` before invoking it. Pending submissions and later draft
edits are separate state: acknowledgment clears only the submitted version.

The request preserves exact UTF-8 body bytes, an optional **full** author and
event ID, and a `BTreeSet<PersonaTag>`. Resolve an internal short reference to its
retained signed envelope before persisting that full target. The database checks
the full ID, author and social-post kind before shortening it for the wire.
Successful retries bypass target validation and return the original signed
event, not a replacement or current materialized post. Different input under
the same key returns `DbError::SocialPostRequestMismatch`.

## Atomicity and cancellation

A preflight lookup avoids normal retry preparation. The authoritative lookup,
target validation, envelope processing, transaction-local payload reservation,
guarded content reduction and receipt insertion share one serialized database
transaction. Admission refusal rolls back everything, including the header.
This deliberately does not use the ordinary public `reserve_payload`, which can
commit a header before payload ingestion. Existing post-commit publication and
buffer/reservation accounting remain in force.

The low-level `Database::commit_social_post_request` seam trusts its in-process
publisher to encode exactly `request.to_social_post()` into the verified
candidate. It does not independently rebind payload bytes to the input digest;
the supported Client operation owns that construction. This is not an ingress
API for untrusted or network-supplied candidates.

Concurrent calls may prepare and sign different candidates, but only the winner
is durably ingested. There is no promise to sign only once. No worker retries on
the caller's behalf. Storage errors, a post-commit panic, task cancellation or
process death can leave an uncertain acknowledgment: retry the **same** key and
input. Synchronous blocking transactions cannot be interrupted by dropping an
async waiter. Shutdown rejects new calls, but an already started call may commit;
hosts must release outstanding calls/owners before shutdown completion closes
the database. There is no rollback-on-shutdown promise.

This guarantees local acceptance under the storage engine's durability
assumptions, not remote delivery, cross-device deduplication, or recovery after
loss, rollback or independent restoration of the database. Tests exercise actual
transaction, post-commit hook and disk reopen boundaries, not arbitrary power
failure.

## Canonical V1 identity

The receipt stores only a versioned 32-byte BLAKE3 digest and original signed
event. This intentionally supersedes an initially considered exact-plaintext
journal: retaining a permanent copy of deleted/pruned text was rejected.
Equality relies on collision and second-preimage resistance, not mathematical
bytewise comparison of retained inputs.

Use BLAKE3 derive-key context `rostra.social_post_idempotent.input.v1` over:

1. Version byte `01`, then fixed text-mode byte `00`.
2. Raw full account RostraId (32 bytes).
3. Body UTF-8 length (u64 big-endian), then exact body bytes.
4. Reply option byte `00` or `01`. If present, full author RostraId (32 bytes)
   and full EventId (32 bytes).
5. Tag count (u64 big-endian), then each tag's UTF-8 length (u64 big-endian) and
   bytes in Rust String/BTreeSet lexical order.

No trimming or normalization occurs. Tags have set semantics, not caller
insertion-order semantics. Request ID is the table key, not digest input.
V1 decoding and hashing must remain stable across future API versions.

## Retention and migration

Schema 34 introduces an empty built-in authoritative receipt table. It invents
no receipts for historical posts; older binaries reject the newer schema.
Total migration keeps the table byte-for-byte **in place**, including throughout
stash creation and recovery, rather than rebuilding it from events/projections.

Receipts never expire and have no individual erase/reset API. Deletion, edits and
payload pruning neither erase receipts nor cause retry to rematerialize content.
Each successful request adds a fixed-size row outside payload quotas, so total
row count remains unbounded. Failed precommit operations add no row.

There is no reversible body/target/tag copy in the receipt, but the signed header
reveals event identity, author, timestamp, parents, content hash and size. The
digest permits offline guesses of low-entropy inputs. It is not encryption,
anonymity or forensic erasure. Whole-account DB deletion ends the guarantee;
backups and ordinary replicated event storage have independent lifetimes.
