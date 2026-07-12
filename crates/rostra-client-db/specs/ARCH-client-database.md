# ARCH-client-database: Per-identity state and projections

`rostra-client-db` is the authoritative state and projection layer for one
local Rostra identity during a database instance's lifetime. It stores
verified graph data, tracks incomplete replication, and materializes
content-specific views used by clients and user interfaces. Disk-backed
instances preserve this state across runs; light clients use an in-memory
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

Table definitions and migrations are implementation details of this crate.
Public database methods and subscription channels form its boundary with the
client and presentation layers.

## Transaction and notification boundary

Event insertion and any immediately available content processing update the
graph, lifecycle bookkeeping, and derived indexes within redb transactions.
Derived side effects must not become visible without the corresponding source
event state.

Notifications are registered on `WriteTransactionCtx` and run only after a
successful commit. Watch channels publish current identity-scoped state such
as heads and follow relationships; broadcast or deduplicating channels signal
new content and work queues. Consumers must treat these channels as wakeups or
incremental observations and use database state as the authority.

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
- Locally imposed payload limits may prune content without removing the event
  envelope or breaking graph traversal.

The detailed payload state machine is specified by
[SPEC-event-content-lifecycle](SPEC-event-content-lifecycle.md). The
implementation-oriented table and flow guide remains in
[docs/content-lifecycle.md](../docs/content-lifecycle.md).
