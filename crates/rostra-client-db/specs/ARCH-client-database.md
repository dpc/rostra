# ARCH-client-database: Per-identity state and projections

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

The database owns an append-only ordinary SocialPost materialization feed. Each
successful ordinary projection appends one dense database-local sequence row
containing its `ShortEventId` in the same transaction. The bounded public scan
resolves each identity against current lifecycle state and returns high-level
present content or a removed marker. This feed is a durable delivery journal,
unlike mutable timeline indexes and lossy broadcasts.

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
value. Public current-state subscriptions return owned snapshots rather than
Tokio watch borrows, so consumers can retain a snapshot across awaits or
database operations without blocking publication. Cloning a subscription
creates an independent change cursor over the same retained state. Broadcast or
deduplicating channels remain lossy or incremental signals for new content and
work queues. The database remains authoritative; these watch payloads are
retained current-state projections.

Materialization cursors identify the next sequence a consumer is waiting for.
They belong to one database lineage and have no cross-lineage meaning. Consumers
durably handle a complete page before checkpointing its `scanned_through`
position. Snapshot exhaustion does not prevent a later append.

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
deletion transaction. An absent reverse row is a no-op when deletion ordering
prevented the ordinary post projection from being materialized.

## Total rebuild

Schema version 25 performs one total rebuild of every earlier production
database before derived version-24 rows are decoded. The preparation transaction
stashes retained signed event headers, available hash-keyed content, the database
identity and iroh secret, initialization time, per-event acquisition source,
canonical forward replacement metadata, and the source version needed for legacy
content decoding. Caller-owned extension tables remain untouched. It deletes all
other reserved built-in state and creates the current schema.

Replay trusts that retained rows crossed the authentication boundary during
normal ingestion; it is not a cryptographic integrity scrub. Typed decoding and
the payload hash-and-length check required to construct verified content still
apply. A separate explicit audit may reauthenticate retained source.
Malformed value decoding returns an error and preserves the stash. The current
storage wrapper still validates range keys infallibly and has no migration-local
allocation limit for corrupt encoded length prefixes; trusted-filesystem
corruption can therefore panic or exhaust memory before replay returns an error.
Recovery for that accepted non-adversarial corruption case is restoring the
pre-upgrade backup or using a separate audited repair tool.

Replay uses two stable `ShortEventId`-ordered streaming passes. The first inserts
every envelope and establishes complete graph, deletion, pruning, and missing
state. The second processes each available eligible payload. This phase boundary
is required because a deleting envelope must be known before content-derived
state is rebuilt; no parent topology or ordering is required within a pass.
Receipt indexes preserve semantic membership and acquisition source but use
authored timestamps under one uniform rebuild policy. Preparation deliberately
discards available envelope receipt timestamps, while content-specific effective
receipt times cannot generally be reconstructed. Allocator values remain
database-local and noncanonical.

Preparation and replay are separate transactions. Preparation commits the fresh
schema and complete reserved stash. Replay and stash cleanup commit together, so
failure rolls back all rebuilt state and the stash forces an identical retry at
the next open. Replay suppresses incremental publication hooks and refreshes
current-state watches once before the database becomes visible.

Total rebuild preserves an existing materialization feed byte-for-byte and
suppresses occurrence emission while replay reconstructs current projections.
A rebuild from a schema predating the feed creates it empty. The version-26
incremental upgrade likewise performs no backfill, so only materializations
committed after cutover appear.

Replay does not retain the event graph or per-event publication closures.
Application and codec code transiently hold the current source record, decoded
below-limit payload, payload clones, and encoding scratch space. Follow-history
pruning uses fixed 256-key batches. Runtime includes B-tree work proportional to
events and references plus hashing/decoding proportional to total referenced
payload bytes, even when many events share one stored payload. The final watch
refresh allocates proportional to self followees, followers, and two-hop WoT.

The backend still runs one long atomic write transaction. redb tracks dirty,
allocated, and freed pages in process memory while retaining copy-on-write
rollback pages on disk, so total RAM and peak disk usage can scale with the
rebuilt database despite streaming application traversal. There is no resource
preflight or safe fixed free-space multiplier. Operators must measure a
production-shaped copy, provision monitored RAM and disk headroom, and retain a
restorable backup. Replay delays open and does not compact the file.

Release engineering must ship the stacked version-24 schema changes and this
version-25 rebuild as one deployable unit. An intermediate ancestor that still
reports version 24 must never be deployed against production storage: it can
write decode-incompatible derived rows without the final rebuild gate. Once
preparation commits version 25, rollback requires restoring the pre-upgrade
backup; an older binary cannot open the database.

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
  acquisition source and semantic membership, not historical reception
  timestamps or sequence values: rebuilt entries use authored timestamps as the
  deterministic fallback because their original local receipt times are not
  retained. Social-post replay also rebuilds the exact event-to-reception-key
  reverse mapping.
- Total migration preserves canonical forward social-post replacement rows and
  rebuilds their reverse lookup index.
- Total migration preserves caller-owned extension tables byte-for-byte without
  replaying or validating their contents.
- Materialization-feed rows are never removed, reordered, or reused. Deletion,
  pruning, and replacement change scan-time resolution to `Removed`; an
  applicable replacement has its own occurrence. Missing envelopes, impossible
  lifecycle state, or invalid processed content are corruption and fail a scan
  without returning an acknowledgment.
  Replacement is classified only after validating lifecycle and, when lifecycle
  claims content remains processed, retained content. Replacement metadata
  cannot mask corruption.

The detailed payload state machine is specified by
[SPEC-event-content-lifecycle](SPEC-event-content-lifecycle.md). The
implementation-oriented table and flow guide remains in
[docs/content-lifecycle.md](../docs/content-lifecycle.md).
