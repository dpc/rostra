# SPEC-event-content-lifecycle: Event content processing

## Record justification

The lifecycle spans event and content tables, reference counting, fetch
scheduling, ingestion transactions, projection processing and reversion, and
client retrieval. Local documentation beside any one component cannot describe
the complete state machine and its cross-table invariants.

Event envelopes and payload bytes have independent arrival and retention
lifecycles. The database preserves the event graph while fetching, processing,
deduplicating, pruning, or deleting payloads. This behavior implements
[SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md) within
[ARCH-client-database](ARCH-client-database.md).

## States

Every inserted event has one effective content state:

- **Missing** means payload processing is still required and includes fetch
  attempt scheduling.
- **Processed** is represented by no content-state entry and means all
  content-derived side effects were applied.
- **Invalid** means payload validation or kind-specific processing failed.
- **Deleted** records an author's graph-level deletion instruction.
- **Pruned** records a local decision not to retain or process the payload.

An event with empty content is processed at insertion and does not enter
Missing. Payloads above the local maximum are pruned. A deletion instruction
can arrive before the target event; when the target later arrives it starts in
Deleted without becoming Missing.

## Processing and idempotency

Envelope insertion is idempotent. On first insertion, content that is neither
deleted nor pruned contributes one reference to its content hash. If usable
bytes are not already present, the event enters Missing and is scheduled for
retrieval. If matching bytes are present, content processing may proceed
without another network fetch.

Only Missing content is eligible for first-time processing. Successful
processing validates the payload, applies kind-specific projections, stores
the bytes by content hash, removes fetch scheduling, and transitions to
Processed. Repeated delivery of an envelope or payload must not duplicate
reference counts, reply counts, follow changes, or other projections.

Projections that retain one latest source event use the maximum
`(event.timestamp, ShortEventId)` defined by
[SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md). Follow and
unfollow, profile, generic singleton, and individual-vote reducers all use this
order. A vote changes its aggregate only when it wins that same comparison, so
the aggregate and retained vote cannot select different equal-second events.

Invalid content is not stored and transitions to Invalid. Deletion, pruning,
or invalidation removes the event's reference to the content hash exactly
once. A later transition from Invalid or Pruned to Deleted records the
author's stronger deletion intent without decrementing again. When deletion
invalidates an already processed social post, the database reverts its
post-specific projections. Derived projections for other content kinds are
currently retained.

Deletion intent is monotone: once a valid direct deleting child has been
observed, ordinary child references and later lifecycle changes cannot erase
it. If several direct same-author children delete the same target, `deleted_by`
is the child with the maximum `(event.timestamp, ShortEventId)`. A deleting
child remains a candidate even when its own content is deleted. This attribution
is direct rather than transitive; in a chain where D2 deletes D1's content and
D1 deletes T's content, T is deleted by D1 and D1 is deleted by D2.
Attribution-only winner changes do not repeat reference-count, fetch-queue,
usage-accounting, or projection-reversion effects.

## Deduplication and retrieval

Content bytes are keyed by hash and may satisfy multiple events, but each
event's content-derived effects are processed independently. Reference counts
track how many events still want a hash. A zero count makes bytes eligible for
garbage collection; reaching zero does not itself require immediate removal.

Missing payloads are ordered by their next fetch time. New work is eligible
immediately and wakes the client fetcher after commit. Failed attempts update
attempt metadata and a later retry time; retry policy is chosen by the client,
while the database owns the schedule and state transition.

The crate's [content lifecycle guide](../docs/content-lifecycle.md) documents
the current tables, detailed flows, and test map. It must remain consistent
with this specification.
