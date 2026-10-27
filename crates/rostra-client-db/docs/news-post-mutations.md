# Durable own-news body corrections

`Client::mutate_news_post_idempotent` takes a persisted random
`NewsPostMutationRequestId` and immutable `NewsPostMutationRequest`.
`expected_current` is the full retained event ID. `NewsPostMutation::Edit { body }`
preserves exact nonblank UTF-8; `Delete` is an explicit operation. Persist the
**news API discriminator** together with the key and input. Never infer it from
current target state when retrying. Legacy text inputs keep using the unchanged
`mutate_social_post_idempotent` API and V1 receipt policy.

## Supported targets and existing wire semantics

`Database::prepare_news_post_mutation(full_id)` returns an advisory snapshot of
the original `SocialPost`. It neither reserves nor refreshes a target. The
authoritative transaction repeats its full-ID, ownership, kind, nonsingleton,
available-payload and root-canonical lineage checks. Each backward/forward walk
allows at most 1024 edges and fails rather than truncating. Canonical fork
selection uses lowest immediate short ID, not timestamps.

The strict, at-most-1,000,000-byte CBOR decoder accepts only known typed
`p/c/r/e/u/h/n/t` fields, `n=true` and no reaction. Unknown or duplicate fields,
trailing data and malformed values are unsupported. Optional original body,
title, URL, legacy persona, declared tags and reply references remain supported.
Indefinite maps are accepted; chunked strings are not. Typed meanings, not
original CBOR ordering/encoding or URL spelling, are preserved. An existing URL
does not authorize navigation or automatic fetching.

Edits change only `c`, without invoking the emoji-reaction heuristic or replacing
legacy persona with effective tag fallback. Title-only originals with absent,
empty or whitespace bodies can receive a nonblank correction. Blank edits are
rejected: the existing reducer treats a deleting header with a blank/absent body
as deletion **even if news metadata remains**. Supporting body clearing as a
title-only replacement would require a separate wire-semantics decision and is
not implemented.

Explicit Delete emits the same fresh plain blank text shape as web deletion:
no news flag, title, URL, reply, reaction or declared tags (legacy persona is the
ordinary new-text default). It is not a malformed blank-news loophole. The
target's retained replacement/reply lineage survives independently; deleting a
post does not delete its children. The tombstone is not a new canonical news
version. Existing payload availability and tombstone classification stay intact.

News replies are eligible for correction because the web edit path preserves
`reply_to` before adding existing news fields, and the existing reducer handles
reply and news projections independently. This does not broaden voting/ranked
read eligibility: those APIs still require top-level supported news.
Historical replies with absent `c` did not contribute a reply count; adding a
body makes them counted, while deleting them follows the existing inverse rule.

## Atomicity, retries and projections

The initial read and serialized write resolve a matching receipt before current
validation, target state, candidate decoding or admission. Same-key/different
input returns `NewsPostMutationMismatch`; target and candidate failures reuse
`SocialPostMutation*` and `SocialPostCandidate`. Full candidate metadata is
independently compared with freshly prepared target metadata before ingestion.

Concurrent requests for the same expected version cannot both win locally.
Same-key callers may independently sign candidates; only the original accepted
event is ingested. Retries return its original signed envelope after later
edits/deletes/pruning, without reingestion or resurrection. A preparation failure
rechecks receipts to reconcile a competing commit.

Admission, target deletion, ordinary counters, reply lineage, news rank indexes,
rank-generation invalidation and receipt insertion share the existing atomic
transaction. Original votes retain their exact historical event target; no vote
is migrated to the replacement. A fresh ranked traversal sees the replacement
and its own votes; old cursors must refresh.

This is local acceptance, not distributed CAS, delivery or present eligibility.
Later remote events may supersede/delete the result. A canceled waiter or lost
acknowledgment does not establish rollback; serialize pending intentions and
retry exactly. Closing clients reject even receipt-only new calls. Hosts still
enforce their session/account boundary; runtime activity is not authentication.

## Receipt identity and migration

Schema **42** adds `social_news_mutation_receipts`, separate from text mutations,
news creation and votes, even when request IDs coincide. Older binaries reject
schema 42. There is no backfill or destructive migration. Total rebuild and
stash/reopen recovery preserve the authoritative table byte-for-byte, not by
replaying events.

The V1 digest uses BLAKE3 derive-key context
`rostra.news_post_mutation.input.v1` followed by version byte `01`, raw account
ID (32 bytes), raw expected event ID (32 bytes), operation (`00` Edit / `01`
Delete), and for Edit only the exact UTF-8 length as u64 big-endian then bytes.
The random 16-byte request ID is the table key, not digest input. This encoding
and admission policy must remain stable for V1.

Permanent receipts store only a versioned digest and original signed envelope.
They survive payload deletion/pruning outside payload quotas; total row count is
unbounded. Digests allow offline guesses, not confidentiality or forensic
erasure. The guarantee is local to this database lineage and ends with loss,
rollback or independent restoration. It does not promise arbitrary OS-kill or
power-loss recovery beyond storage-engine durability.
