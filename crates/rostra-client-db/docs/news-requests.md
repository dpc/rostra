# Durable news requests and generation-bound ranked traversal

These are local APIs over the existing SocialPost and SocialVote wire formats.
They neither add a protocol intent marker nor reinterpret historical reducers.

## Immutable input and permanent receipts

`Client::social_news_post_idempotent(secret, NewsPostRequestId, &NewsPostRequest)`
accepts exact body, title and optional URL strings. Normalize the title and URL
**before** persisting the random 128-bit request key and immutable input. Titles
must be nonempty and already trimmed. Optional URLs must be nonempty, already
trimmed and parseable. Bodies may be empty. The emitted post has fixed `{news}`
persona tags, `news=true`, and no reply or reaction. There is no configurable tag
or reply field. The digest binds exact URL spelling, while the signed payload
contains the parsed URL's serialization. Parsing or normalization must not be
repeated by the host to modify an already-pending request.

`Client::set_social_vote_idempotent(secret, VoteRequestId, &VoteRequest)` accepts
an exact `(full RostraId, full EventId)` and `Option<bool>`: up, down, or clear.
The full event ID is recomputed from the retained envelope before shortening;
the full author must match. There is **no replacement-history retargeting**.
New up/down/clear requests all require an available, ordinarily materialized,
supported news target. An available news replacement can itself be targeted;
a deleted or pruned historical version cannot be voted on through this API.

Supported news is a bounded, strictly typed CBOR map with known fields, no
duplicates or trailing bytes, `news=true`, no reply and no reaction. Historical
optional title/body/URL/persona fields remain supported; singleton posts do not.
This policy is narrower than permissive signed-event ingestion. It does not
change replay. A historical unsupported URL is data, not permission to navigate:
native hosts must keep non-HTTP(S) or credential-bearing URLs inert.

News and vote keys live in **separate** account-local namespaces, also separate
from text, edit/delete and reaction requests. The first accepted request stores a
versioned digest and original signed event. Both the initial read and the
authoritative write look up receipts before parsing input or checking mutable
target state. Same-key/same-input retries return that event even after deletion,
pruning, replacement, or a newer vote. They do not reingest content or reapply a
singleton. Different input returns `NewsRequestMismatch` or
`VoteRequestMismatch`, even when that different input is now invalid.

Admission, event graph changes, payload reduction, rank/vote projections and
receipt insertion share one serialized database transaction. Precommit failure
rolls them all back. Notifications run after commit using the existing ordered
publication boundary. Payload admission and buffer accounting remain in force;
receipt rows are fixed-size metadata outside payload quotas.

## Vote ordering and acknowledgment

Existing singleton precedence is `(signed timestamp, ShortEventId)`, not arrival
order. The request ID must **not** replace the vote's auxiliary singleton key:
that key remains the target's shortened EventId. A new local vote is accepted
only with an actual wall timestamp strictly greater than the retained winner.
The database rechecks this inside the admission transaction; merely checking
before signing is insufficient. A race retries within the client's two-second
ordering budget. Same-second requests wait for wall time to advance, not an
arbitrary event-ID tie-break and not a synthetic future timestamp.

The budget bounds waiting for ordering, not database I/O or total completion.
`VoteOrderingBusy`, `VoteClockBehind` and `VoteTimestampExhausted` reject this
call without accepting its candidate. The transaction also rejects a candidate
ahead of current wall time (`VoteFutureTimestamp`), including a clock rollback
between preparation and admission. Durable singleton timestamps survive restart
and source-payload pruning; MAX never wraps. An already accepted receipt
reconciles before these checks. Local request errors recheck receipts to
reconcile a concurrent same-key acceptance.

These guarantees are **local acceptance**, not remote delivery, cross-device
deduplication or cross-device compare-and-swap. Later same-account device events
can supersede the result under ordinary singleton ordering. A future remote
winner can prevent new local votes until the clock catches up. A returned old
receipt says nothing about the current displayed vote.

Input/target/unsupported/admission/ordering rejections mean this call did not
accept a candidate; another independently running same-key call may still
commit. Storage errors, post-commit hook panic, process death, shutdown racing an
already-started transaction, and cancellation can have **uncertain** outcomes.
Dropping an async waiter does not cancel an executing synchronous transaction.
Persist and retry the same immutable key/input; serialize unresolved user
intents rather than sending a newer intent after uncertainty. New calls,
including receipt-only retries, are refused once client shutdown begins.

## All-depth ranked traversal

`Database::paginate_news_ranked(cursor, limit)` returns full bound target IDs,
supported news content, rank/vote metadata, self vote, and a continuation.
Page sizes are 1..=100. There is **no total-row capture cap**: an unchanged feed
can be traversed beyond any fixed number of pages.

The opaque serializable `NewsFeedCursor` binds a per-open database nonce, a
rebuild epoch and checked monotonic generation, and an exclusive descending
`(score, ExternalEventId)` position. The generation comparison and page scan
share one read transaction. Successful continuation therefore preserves
ordering and membership. Rank movement, insertion, removal, materialization,
deletion, replacement, pruning and aging-out invalidate continuation. Reopen
changes the instance nonce; total rebuild replaces the epoch even within one
Database instance. Generation overflow aborts the modifying transaction instead
of wrapping. Random incarnation uniqueness has the usual 128-bit collision
assumption; cursors are transport data, not authority or permanent bookmarks.

`NewsCursorStale` returns no rows and never silently restarts at page one.
Hosts must retain loaded pages and offer explicit Refresh. In an actively
changing feed, repeated expiry can indefinitely prevent reaching the end.
There is no omission-free traversal promise across invalidations and no pinned
database snapshot or retained snapshot registry.

Membership/order is not a frozen cross-page content/count snapshot. Immutable
signed content is read in each page, and reply counts, vote sums and self-vote
metadata may differ between page reads without invalidating the cursor. A
vote-sum change invalidates order only when the existing asynchronous score
recalculation actually changes the rank. Unrelated writes, duplicate ingestion
and unchanged score recalculation do not invalidate. Materialization upserts
conservatively invalidate even if an index key already exists.

Each scan consumes at most 10000 raw index entries, plus one unconsumed boundary
probe, and examines at most 16 MiB of encoded content. Supported content has a
1,000,000-byte (decimal MB) per-payload limit, matching the strict decoder's
policy. Larger historical news remains valid signed wire data but is outside
this native interface: it is skipped without loading the bytes and reported in
`oversized_rows`, a subset of `unsupported_rows`. The latter also counts missing,
unavailable, identity-mismatched and unsupported-shape entries. This is an
explicit presentation limitation, not a protocol rejection.

`NewsFeedStop` distinguishes `End`, `PageLimit`, `ScanBudget` and `PayloadBudget`.
The raw-position continuation consumes every examined unsupported row, so
arbitrarily sparse spans and repeated oversized rows cannot permanently block
later supported content. At a cumulative-byte boundary the next row remains
unconsumed and fits on the next call. Empty non-End pages still make progress:
hosts should offer Continue scanning or perform only bounded background
continuation, never infer an empty feed from an empty page.

`raw_rows_scanned` reports consumed entries; the boundary probe may read one more
index key/envelope but never its payload. `next_cursor` means more **raw
scanning**, not a promise of another eligible row. Only End/no cursor denotes
exhaustion in that snapshot. Stale, storage, malformed internal-state and invalid
page-size errors still return no partial result. Limits bound work/input/output,
not a CPU deadline or process RSS; hosts should bound concurrency.

The mutation audit has three ownership points in `news.rs`: rank upsert (ordinary
materialization, including replacement/restoration), removal (ordinary content
reversion, including deletion and quota pruning), and recalculation (changed
score or age removal). Each advances generation in the same transaction.
Reducers and quotas must continue using these seams. No other production code
directly modifies rank membership/order.

## Schema, digest and privacy

Schema **37** creates empty `social_news_receipts`, `social_vote_receipts`, and a
disposable `social_news_generation` table. There is no historical receipt
backfill. Older binaries reject this schema. Incremental upgrade rebuilds only
the disposable score index from the unchanged per-post rank records: old signed
integer keys used zigzag encoding under a bytewise comparator, which interleaved
negative scores with positive magnitudes. The new key is exactly eight big-endian
bytes of `(score as u64) XOR 2^63`, followed by the existing target key. Public
scores and cursors remain signed numbers. Both ranked pagers now use numeric
descending order, including extrema, width boundaries and equal-score ties.
Index replacement and schema advancement share a transaction, so failed upgrade
does not leave a half-rebuilt index or newer schema marker.
Total migration preserves both receipt tables byte-for-byte
in place, including interrupted stash/reopen recovery; it does not derive them
from signed content. Built-in prefixes protect all three tables from extension
transactions.

News digest V1 uses BLAKE3 derive-key context
`rostra.news_post_request.input.v1`, then byte `01`, raw account ID (32 bytes),
body and title each encoded as u64 big-endian UTF-8 byte length followed by exact
bytes, then URL option byte `00`/`01` and, if present, its length and exact bytes.
The fixed no-reply/news persona defaults are implicit in this version's domain.
Vote V1 uses `rostra.vote_request.input.v1`, byte `01`, raw account ID, full
target author, full target EventId (32 bytes each), then value byte `00` clear,
`01` down, or `02` up. Request ID is the table key, not digest input.

News's nonsingleton auxiliary nonce uses derive-key context
`rostra.news_post_request.event_nonce.v1` over raw account ID followed by request
ID (16 bytes), taking the first 16 digest bytes. Existing reducers ignore this
key when the singleton flag is clear. Distinct requests therefore cannot
silently collapse equal head/time/body publications. Admission additionally
rejects an already retained candidate without a matching receipt. Votes never
use this nonce scheme: strict timestamp advancement supplies distinct events.

These encodings/domains must remain stable for V1 receipts. Receipts retain no
reversible copy of body, title, URL, or vote input, but digests permit offline
low-entropy guesses and signed metadata remains visible. Equality assumes
BLAKE3 collision/second-preimage resistance. Receipts never expire, have no
individual erase/reset operation, and their total row count is unbounded.
This is not encryption or forensic erasure. Whole-account deletion, loss,
rollback or independent restore ends the guarantee. Ordinary replay may
rematerialize retained non-quota-pruned content independently of receipt lookup.
