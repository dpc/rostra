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
content, query graph and social state, and subscribe to state changes. They do
not mutate redb tables directly.

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

## Transaction and notification boundary

Event insertion and any immediately available content processing update the
graph, lifecycle bookkeeping, and derived indexes within redb transactions.
Derived side effects must not become visible without the corresponding source
event state.

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
  winner as the individual-vote projection.
- Locally imposed payload limits may prune content without removing the event
  envelope or breaking graph traversal.
- Per-identity usage accounting is authoritative for retained envelopes and
  their payload lifecycle. Every accepted payload contributes once to total
  usage and exactly one of current, missing, deleted, pruned, or invalid usage,
  including payloads whose envelopes arrive already Deleted.
- Total migration rebuilds reception-order indexes and their sequence from
  retained event envelopes and available retained content. It preserves semantic
  membership, not historical reception sequence values.
- Total migration preserves canonical forward social-post replacement rows and
  rebuilds their reverse lookup index.

The detailed payload state machine is specified by
[SPEC-event-content-lifecycle](SPEC-event-content-lifecycle.md). The
implementation-oriented table and flow guide remains in
[docs/content-lifecycle.md](../docs/content-lifecycle.md).
