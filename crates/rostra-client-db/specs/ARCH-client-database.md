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
successful commit. Watch channels retain the latest committed identity-scoped
projection, including the self head, followees, followers, and Web of Trust.
Commit and watch publication are ordered together, so an older transaction
cannot overwrite a newer projection; subscribing after a period with no
receivers still yields the latest committed value. Broadcast or deduplicating
channels remain lossy or incremental signals for new content and work queues.
The database remains authoritative; these watch payloads are retained
current-state projections.

Reception order is database-local state, not a replicated ordering contract.
One durable sequence supplies all reception-order indexes, and each allocation
commits atomically with its index insertion. Aborted transactions do not consume
sequence values, and an occupied index key causes the transaction to fail rather
than replace an existing member. Different replicas need not assign the same
sequence to an event. Sequence exhaustion fails the transaction instead of
wrapping and reusing values.

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
  projections; other content kinds currently retain their derived
  projections.
- Social-post replacement lineage remains available across a Deleted
  intermediate. Verified Deleted content may establish only immutable edit
  metadata, whose canonical forward rows survive total migration; it cannot
  apply ordinary content projections or change lifecycle bookkeeping.
- Follow state, profiles, generic singletons, and individual votes select the
  maximum `(event.timestamp, ShortEventId)`. Vote aggregates use the same
  winner as the individual-vote projection. An existing vote winner must
  resolve to its source event and a vote for the same target. A terminal
  Deleted or Pruned source remains resolvable while its verified content bytes
  are retained because those states retain non-post projections;
  otherwise the database is corrupt and the replacing transaction aborts
  without changing the aggregate.
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
  their original local receipt times are not retained.
- Total migration preserves canonical forward social-post replacement rows and
  rebuilds their reverse lookup index.
- Total migration preserves caller-owned extension tables byte-for-byte without
  replaying or validating their contents.

The detailed payload state machine is specified by
[SPEC-event-content-lifecycle](SPEC-event-content-lifecycle.md). The
implementation-oriented table and flow guide remains in
[docs/content-lifecycle.md](../docs/content-lifecycle.md).
