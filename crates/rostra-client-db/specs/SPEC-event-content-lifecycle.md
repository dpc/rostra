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

The public high-level content-ingestion boundary accepts either arrival order.
`VerifiedEventContent` carries its verified envelope, so content ingestion first
inserts that envelope when it is absent and then applies the ordinary content
lifecycle in the same transaction. Supplying the envelope separately before or
afterward is idempotent. Existing-envelope-only transaction helpers are internal
implementation and migration interfaces, not caller preconditions.

## States

Every inserted event has one effective content state:

- **Missing** means payload processing is still required and includes fetch
  attempt scheduling.
- **Processed** is represented by no content-state entry and means all
  content-derived side effects were applied.
- **Invalid** means payload validation or kind-specific processing failed.
- **Deleted** records an author's graph-level deletion instruction.
- **Pruned** records a local decision not to retain or process the payload.

Unless it starts in Deleted, an event with empty content is processed at
insertion and does not enter Missing. The local maximum payload length is
inclusive: non-Deleted payloads at or below the maximum are eligible for
processing, while those above it are pruned. An already-Deleted payload remains
Deleted and is ineligible regardless of size. A deletion instruction can
arrive before the target event; when the target later arrives it starts in
Deleted without becoming Missing or contributing a content reference.

## Processing and idempotency

Envelope insertion is idempotent. On first insertion, content that is neither
deleted nor pruned contributes one reference to its content hash. If usable
bytes are not already present, the event enters Missing and is scheduled for
retrieval. If matching bytes are present, content processing may proceed
without another network fetch.

Only Missing content is eligible for first-time processing. Successful
processing validates the payload, applies kind-specific projections, stores
the bytes by content hash, removes fetch scheduling, and transitions to
Processed. The sole terminal-state exception is immutable replacement-lineage
extraction from a verified, well-formed Deleted `SOCIAL_POST` payload, as
defined below. Repeated delivery of an envelope or payload must not duplicate
reference counts, reply counts, follow changes, or other projections.
Eligibility is established before fetch scheduling is removed, so rejected or
terminal-state payload delivery cannot orphan a Missing event.

Projections that retain one latest source event use the maximum
`(event.timestamp, ShortEventId)` defined by
[SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md). Follow and
unfollow, profile, generic singleton, and individual-vote reducers all use this
order. A vote changes its aggregate only when it wins that same comparison, so
the aggregate and retained vote cannot select different equal-second events.
Before replacing an existing vote winner, its singleton row must resolve to the
stored event with the same ID, author, kind, singleton/auxiliary-parent shape,
timestamp, verified retained content, and vote target. The source must have
reached Processed; a later Deleted or Pruned state remains resolvable while
those verified bytes are retained because non-post projections survive those
transitions. Missing, Invalid, absent/mismatched content, or any relationship
mismatch is database corruption and aborts the complete ingestion transaction,
leaving the aggregate, winner, and newly submitted envelope unchanged.

Invalid content is not stored and transitions to Invalid. Deletion, pruning,
or invalidation removes the event's reference to the content hash exactly
once. A later transition from Invalid or Pruned to Deleted records the
author's stronger deletion intent without decrementing again. When deletion
invalidates an already processed social post, the database reverts its
post-specific projections. Derived projections for other content kinds are
currently retained.

An at-or-below-limit `SOCIAL_POST` that has the delete-auxiliary-parent flag,
has an auxiliary parent, decodes successfully, and has `djot_content` whose
trimmed body is nonempty is an edit. It records exactly two immutable,
author-scoped replacement rows: a forward row that is both canonical source
metadata and a lookup index, and a reverse lookup row. Total migration
preserves the forward row and rebuilds the reverse row from it. Missing, empty,
or whitespace-only `djot_content` is a deletion, even
when other social-post fields are populated, and records no replacement
metadata. Replacement lookup follows edges transitively: in `E <- A <- B`,
resolving E yields newest B even when A's content became Deleted before A's
envelope or payload arrived.

Supplying hash-verified bytes for a Deleted event, or finding such bytes
already in the local deduplicated store when its envelope arrives, may only
establish that replacement metadata when the exact edit predicate above holds.
The database preserves the forward row across total migration and reconstructs
the reverse index from it; it does not retain newly supplied Deleted
bytes for replay. Deleted state continues to block content retrieval. The
database never schedules or requests bytes for a Deleted event.

This exception does not change content state, reference counts, fetch
scheduling, usage accounting, singleton or vote state, visibility indexes,
reply or reaction projections, news ranking, mentions, or notifications.
Over-limit, blank-body, malformed, non-social, and non-edit Deleted payloads
derive no metadata, and supplied bytes from these paths are not stored.

Deletion intent is monotone: once a valid direct deleting child has been
observed, ordinary child references and later lifecycle changes cannot erase
it. If several direct same-author children delete the same target, `deleted_by`
is the child with the maximum `(event.timestamp, ShortEventId)`. A deleting
child remains a candidate even when its own content is deleted. This attribution
is direct rather than transitive; in a chain where D2 deletes D1's content and
D1 deletes T's content, T is deleted by D1 and D1 is deleted by D2.
Attribution-only winner changes do not repeat reference-count, fetch-queue,
usage-accounting, or projection-reversion effects.

Per-identity payload usage counts every accepted event payload exactly once.
Total payload size and count equal the sums of the current, missing, deleted,
pruned, and invalid buckets. A payload whose envelope starts in Deleted enters
the total and deleted buckets directly; it never enters Missing, changes
content reference counts, or fabricates lifecycle transition side effects.

## Deduplication and retrieval

Content bytes are keyed by hash and may satisfy multiple events, but each
event's content-derived effects are processed independently. Reference counts
track how many events still want a hash. A zero count makes bytes eligible for
garbage collection; reaching zero does not itself require immediate removal.

Missing payloads are ordered by their next fetch time. New work is eligible
immediately and wakes the client fetcher after commit. Failed attempts update
attempt metadata and a later retry time; retry policy is chosen by the client,
while the database owns the schedule and state transition.

In canonical state, each event has at most one fetch-queue row. A current row
exists only for a Missing event, and its timestamp equals that state's
`next_fetch_attempt`. Legacy inconsistent physical rows can remain until they
reach the queue front, but queue APIs never return them as current work. A
failed fetch completion updates the schedule only when the schedule observed
by its caller is still current and the replacement schedule is strictly later;
a stale or non-forward completion has no effect. Strictly increasing schedules
prevent reuse of a timestamp as an ABA-prone compare-and-set token. Fetcher
queue peeking removes inconsistent front rows transactionally before returning
valid work, so stale rows cannot hide later work or cause content that reached
a terminal state to be fetched again. Total migration rebuilds the queue from
retained event and content sources rather than preserving old queue rows.

The crate's [content lifecycle guide](../docs/content-lifecycle.md) documents
the current tables, detailed flows, and test map. It must remain consistent
with this specification.
