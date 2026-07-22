# ARCH-client-database: Per-identity state and projections

## Status

Fresh databases and databases rebuilt from retained events derive the follow
epoch described below. Until the final total-rebuild change tracked as `t4vh`,
a version-24 database already using the compatible stacked-development schema
opens without backfilling follow history or retained unfollow boundaries; its
active-winner fallback cannot fully recover the epoch from legacy rows.
Pre-series production version-24 follow rows use an incompatible encoding and
may not open until `t4vh`. Late follow or unfollow changes also do not rewrite
receipt indexes that were already materialized. The final rebuild supplies the
follow-history backfill; as specified below, rebuilt receipt indexes use authored
timestamps because historical local receipt times are unavailable.

The shortened-identity collision guard applies to new ingestion and total
replay. Existing version-24 databases are not proactively scanned, so a mapping
written before the guard may remain until encountered. The final rebuild tracked
by `t4vh` validates retained event authors under the guard.

New social-post projection insertion and reversion use the symmetric
applicability rule below. Existing version-24 databases are not scanned or
repaired by this staged change: reply or reaction counts that the former
asymmetric blank-post reversion already decremented can remain incorrect until
the final total rebuild tracked by `t4vh`.

Current-schema social-vote singleton rows retain the winning full target and
value inline. Pre-series production version-24 singleton rows use a
decode-incompatible encoding and are not safely usable with the current
singleton projection until `t4vh` rebuilds winners and vote aggregates from
retained source events.

New and total-replay-built social receipt rows have the reverse mapping required
for exact reversion. Existing version-24 `social_posts_by_received_at` rows are
not scanned or backfilled by this staged change. If one of those legacy posts is
deleted before `t4vh`, its unaddressable stale receipt row can remain until the
final total rebuild discards and reconstructs both receipt directions.

`rostra-client-db` is the authoritative state and projection layer for one
local Rostra identity during a database instance's lifetime. It stores
verified graph data, tracks incomplete replication, and materializes
content-specific views used by clients and user interfaces. Disk-backed
instances preserve retained source data and stable identity and initialization
metadata across runs. Total migrations may discard and canonically rebuild
disposable lifecycle and projection state. Light clients use an in-memory
instance. The crate implements the storage role in
[ARCH-rostra](../../../specs/ARCH-rostra.md) for the event contract in
[SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md).

## Ownership and interfaces

Each `Database` is bound to a `self_id`; reopening storage for a different
identity is rejected. The database also owns the iroh node secret associated
with that client database, which is persistent for a disk-backed instance and
transient for an in-memory instance. Higher layers submit verified events and
content, query graph and social state, and subscribe to state changes. They
cannot open built-in redb tables or invoke transaction-level reducers directly.

The database separates:

- event envelopes, parent relationships, heads, and missing-parent tracking;
- content bytes, per-event content state, reference counts, and fetch
  scheduling;
- identity endpoint and follow-graph projections;
- content-kind projections such as social posts, profiles, replies, votes,
  and news scores;
- reception-order indexes used for local timelines and notifications.

The `events_self` index contains every accepted envelope authored by the local
identity. It indexes graph membership rather than content availability:
deletion, pruning, or invalid content does not remove the envelope.

Table definitions and migrations are implementation details of this crate.
Public database methods and subscription channels form its boundary with the
client and presentation layers.

Trusted in-process components that share the database file, such as bots, may
persist caller-owned tables through `Database::extension_read` and
`Database::extension_write`. The extension transaction exposes typed table
operations but not the underlying transaction, built-in table definitions, or
post-commit projection hooks. It rejects every built-in table name. Extension
owners must choose stable component-qualified names outside the reserved
built-in prefixes and own their table schema, data invariants, and
compatibility. Core replay and convergence guarantees exclude caller-owned
extension tables. Total migrations preserve those tables byte-for-byte without
validating or rebuilding them. The bot's existing unqualified table names remain
supported for storage compatibility; new tables use component-qualified names.

## Transaction and notification boundary

Event insertion and any immediately available content processing update the
graph, lifecycle bookkeeping, and derived indexes within redb transactions.
Derived side effects must not become visible without the corresponding source
event state.

Event ingestion records each retained event author in `ids_full`, keyed by the
identity's 128-bit prefix with the remaining 128 bits as its value. Current
consumers reconstruct that author index when enumerating known identities. An
identical mapping is idempotent. A different identity with the same prefix
aborts normal ingestion without replacing the established, first-committed
mapping or changing event state, lifecycle bookkeeping, or projections. During
total migration, a collision rolls back the replay transaction; the separately
committed preparation and retryable source stash remain.

Typed writes use redb-bincode's configured big-endian bincode encoding. Decoding
must consume the complete stored byte slice; a trailing byte is corruption, not
another representation of the same key or value. Range iteration validates keys
before yielding entries, even when the caller does not otherwise inspect a key.

Notifications are registered on `WriteTransactionCtx` and run only after a
successful commit. A per-`Database` write mutex spans transaction creation,
durable commit, and all post-commit hooks. Its primary purpose is to preserve
redb writer order through hook publication order: without this full span, hooks
from separate committed transactions could race and publish older state after
newer state. Watch channels retain the latest committed identity-scoped
projection, including the self head, followees, followers, and Web of Trust.
Subscribing after a period with no receivers still yields the latest committed
value. Broadcast or deduplicating channels remain lossy or incremental signals
for new content and work queues. The database remains authoritative; these
watch payloads are retained current-state projections.

Write closures and post-commit hooks must not synchronously re-enter database
writes: the non-reentrant mutex would deadlock. A hook panic propagates after
the transaction has committed and poisons the mutex; a later write deliberately
recovers the guard and continues, so the committed database remains usable.
Every registered hook is attempted even if one panics, then the first panic
resumes. Callers must not interpret that panic as transaction rollback.

The durable head table is authoritative when an identity has concurrent graph
tips. The retained self-head watch projects that set to its minimum
`ShortEventId`, matching reopen and replay. This value is only a deterministic
representative and default append parent: it carries no freshness, preference,
or uniqueness meaning. Consumers that require every branch read the complete
durable set. The incremental new-head broadcast instead carries the exact
accepted event that became a head and may be recovered from durable state after
lag. Event content readiness is a separate incremental signal because envelopes
can become heads before their payload is available.

Reception order is database-local state, not a replicated ordering contract.
One durable sequence supplies all reception-order indexes, and each allocation
commits atomically with its index insertion. Aborted transactions do not consume
sequence values, and an occupied index key causes the transaction to fail rather
than replace an existing member. Different replicas need not assign the same
sequence to an event. Sequence exhaustion fails the transaction instead of
wrapping and reusing values.

Each current-schema social-post receipt also stores an event-to-reception-key
reverse row in the insertion transaction. Reverting that processed post removes
the forward and reverse rows atomically. Removal does not rewind the shared
allocator or make its sequence value reusable. A present reverse row that does
not resolve to the same event in the forward index is corruption and aborts the
deletion transaction.

## Invariants

- Only cryptographically verified event envelopes enter normal processing.
- Each shortened identity prefix resolves to at most one full `RostraId`;
  a later collision fails closed without replacing the first-committed mapping.
- Duplicate event delivery is idempotent and does not repeat reference-count
  or projection changes.
- Unknown parents and absent payloads are represented explicitly and can be
  completed after out-of-order delivery.
- Content-derived projections are applied at most once for each event
  content. Deletion of a processed social post reverts its post-specific
  projections, including both directions of its notification receipt index;
  other content kinds currently retain their derived projections.
- Social-post projection reversion has the same applicability as insertion.
  A blank or whitespace-only post whose header deletes its auxiliary parent's
  content neither adds nor removes ordinary authored-time, reply, reaction,
  news, self-mention, or reception-order projections. Nonblank edits apply and
  revert those projections normally; counter or receipt-mapping mismatches fail
  closed rather than saturating or mutating unrelated rows.
- Social-post replacement lineage remains available across a Deleted
  intermediate. Verified Deleted content may establish only immutable edit
  metadata, whose canonical forward rows survive total migration; it cannot
  apply ordinary content projections or change lifecycle bookkeeping.
- Follow state, profiles, generic singletons, and individual votes select the
  maximum `(event.timestamp, ShortEventId)`. Vote aggregates use the same
  winner as the individual-vote projection. Each vote winner retains its full
  target and authoritative current-projection `Down`, `Neutral`, or `Up` value
  beside its event ID. A replacement computes its aggregate delta from that
  retained projection. If different full targets share one shortened auxiliary
  key, the winning change transfers the contribution between their aggregates.
  Winner and all aggregate updates commit atomically. Only singleton-shaped
  votes whose auxiliary key matches their payload target enter this coupled
  projection. A cached target whose shortened event ID differs from its retained
  row key is corruption and fails reads and replacement.
  Vote reads and replacements do not require source payload bytes, which may
  legitimately be absent after deletion or pruning makes them
  garbage-collection eligible. Retained signed source remains authoritative
  for replay and explicit audits; a detected source/cache disagreement requires
  quarantine or projection recomputation, never an isolated cache rewrite.
- An active follow's `first_ts` is the timestamp of the earliest follow in the
  current uninterrupted follow epoch, not the first-ever follow. The latest
  unfollow is the exclusive epoch boundary under the same total event order.
  Follow history and that boundary remain available while the relationship is
  active, so a late follow after the boundary can lower `first_ts`, while a
  follow at or before the boundary cannot leak into the current epoch.
- Social-post and shoutbox notification indexes use the event timestamp for an
  active followee's historical content only when the content strictly predates
  both database creation and the current follow epoch's `first_ts`; otherwise
  they use local receipt time. A content timestamp equal to `first_ts` is
  treated as current because the timestamp-only cutoff cannot order
  equal-second content relative to the follow event.
- Locally imposed payload limits may prune content without removing the event
  envelope or breaking graph traversal.
- Per-identity usage accounting is authoritative for retained envelopes and
  their payload lifecycle. Every accepted payload contributes once to total
  usage and exactly one of current, missing, deleted, pruned, or invalid usage,
  including payloads whose envelopes arrive already Deleted.
- Total migration rebuilds reception-order indexes and their sequence from
  retained event envelopes and available retained content. It preserves
  semantic membership, not historical reception timestamps or sequence values:
  rebuilt entries use authored timestamps as the deterministic fallback because
  their original local receipt times are not retained. Social-post replay also
  rebuilds the exact event-to-reception-key reverse mapping.
- Total migration preserves canonical forward social-post replacement rows and
  rebuilds their reverse lookup index.
- Total migration preserves caller-owned extension tables byte-for-byte without
  replaying or validating their contents.

The detailed payload state machine is specified by
[SPEC-event-content-lifecycle](SPEC-event-content-lifecycle.md). The
implementation-oriented table and flow guide remains in
[docs/content-lifecycle.md](../docs/content-lifecycle.md).
