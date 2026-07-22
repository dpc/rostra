# Event Content Lifecycle

> **See also**: `src/tables.rs` for table definitions and inline documentation.
> When updating this document, ensure `tables.rs` stays in sync.

This document describes how events and their content are tracked, processed, and
reference-counted in the Rostra client database.

## Overview

Events in Rostra form a DAG (Directed Acyclic Graph). Each event has an
"envelope" (metadata + signature) and "content" (payload). These are stored
separately to enable:

1. **Content deduplication**: Same content shared by multiple events is stored once
2. **Content pruning**: Large content can be discarded while keeping DAG structure
3. **Out-of-order delivery**: Events can arrive before their content

The public `Database::process_event_content` method also accepts the reverse
arrival order. `VerifiedEventContent` includes the verified envelope, so the
method inserts a missing envelope and processes the content atomically. Calling
`process_event` before or after it, or delivering either item repeatedly, does
not repeat reference counts, usage accounting, lifecycle transitions, or
content-derived projections. The lower-level `process_event_content_tx` helper
assumes an existing envelope and remains internal for transaction composition
and migration replay.

Content may be empty (`content_len == 0`). Empty content is handled as normal
content — it gets an RC entry and is stored in `content_store` immediately at
event insertion time unless the event starts in Deleted.

New ingestion and total replay enforce the exclusive maximum described below.
Existing version-24 rows admitted at the exact limit are not proactively
rewritten without a database-version bump. Their current lifecycle state remains
until duplicate processing or total replay; previously derived replacement
lineage remains until the final total rebuild tracked by `t4vh`.

## Key Tables

| Table | Key | Purpose |
|-------|-----|---------|
| `events` | `ShortEventId` | Main event storage (envelope only) |
| `content_store` | `ContentHash` | Content storage (deduplicated by hash) |
| `content_rc` | `ContentHash` | Reference count per content hash |
| `events_content_state` | `ShortEventId` | Per-event processing state |
| `events_content_missing` | `(Timestamp, ShortEventId)` | Events waiting for content, sorted by next fetch time |
| `social_posts_by_received_at` | `(Timestamp, u64)` | Social posts ordered by effective local receipt time |
| `social_posts_received_at_keys` | `ShortEventId` | Exact reverse key for removing a social receipt without scanning |

## State Machine

Each event's content processing goes through these states:

```
                                ┌─────────────────────────────────┐
                                │                                 │
                                v                                 │
Event Inserted ──► Missing ──► (no entry) ──► Deleted/Pruned     │
                      │               │              │            │
                      │               │              └────────────┘
                      │               │              (content delete after
                      │               │               prune/invalid changes
                      │               │               state, no RC change)
                      ├───────────────┴─► Deleted/Pruned
                      │  (content delete/     (if content deleted
                      │   prune before         before event arrives
                      │   content processing)  via events_missing)
                      │
                      └─► Invalid ──► Deleted
                        (content failed     (author content
                         validation)         delete records intent)
```

Note: Events with `content_len == 0` skip the `Missing` state entirely. They go
straight to "no entry" (processed) during event insertion unless they start in
Deleted.

### State Meanings

| State | `events_content_state` | Meaning |
|-------|------------------------|---------|
| Missing | `Missing { last_fetch_attempt, fetch_attempt_count, next_fetch_attempt }` | Event inserted, content not yet processed |
| Processed | *no entry* | Content processed, side effects applied |
| Invalid | `Invalid` | Content failed validation (e.g. CBOR deserialization) |
| Deleted | `Deleted { deleted_by }` | Author deleted this content |
| Pruned | `Pruned` | Locally pruned (too large, etc.) |

**Key insight**: "No entry" in `events_content_state` means content was
successfully processed. This is the normal state for most events.

## Reference Counting

RC tracks how many events want a particular content hash. This enables garbage
collection when no events need the content.

### RC Rules

1. **Increment**: When event is inserted (all events whose content is not
   already marked for deletion, including `content_len == 0`)
2. **Decrement**: When event content is deleted, pruned, or marked invalid
3. **Never double-decrement**: Guards check if content already Deleted/Pruned/Invalid

### RC and Content Store

- RC is managed at **event insertion time**, not when content arrives
- Content is stored in `content_store` when first processed (or immediately
  for `content_len == 0` events that do not start in Deleted)
- When RC reaches 0, content *can* be garbage collected (not automatic)

## Detailed Flows

### Flow 1: Normal Event Arrival (content_len > 0)

The configured maximum content length is an exclusive upper bound. Content
whose length is below the maximum follows this flow; content at or above it is
pruned during envelope processing and never enters Missing.

```
1. insert_event_tx:
   - Add event to `events`
   - Increment RC for content_hash
   - Mark as Missing { count: 0, next: ZERO } in `events_content_state`
   - Content already in store? Skip adding to `events_content_missing`
   - Otherwise add (Timestamp::ZERO, event_id) to `events_content_missing`

2. process_event_content_tx:
   - Check length eligibility and can_insert_event_content_tx: Missing? → proceed
   - Leave fetch scheduling unchanged if the payload is ineligible
   - Apply side effects (reply counts, follow updates, etc.)
   - Store content in `content_store` (if not already there)
   - Remove from `events_content_missing` (using next_fetch_attempt from state)
   - Remove Missing marker from `events_content_state`
```

### Flow 1b: Non-deleted Empty Content Event (content_len == 0)

```
1. insert_event_tx:
   - Add event to `events`
   - Increment RC for content_hash (blake3 hash of empty bytes)
   - Store empty content in `content_store` (if not already there)
   - Track payload as processed immediately (no Missing state)
   - No entry in `events_content_state` (already "processed")
```

### Flow 2: Event Arrives Before Content

```
1. insert_event_tx (event only):
   - Add event to `events`
   - Increment RC
   - Mark as Missing
   - Add to `events_content_missing` (content not in store)

2. Later, content arrives via another event:
   - Content stored in `content_store`

3. process_event_content_tx:
   - Check Missing → proceed
   - Apply side effects
   - Remove from `events_content_missing`
   - Remove Missing marker
```

### Flow 2b: Content Arrives at the Public Boundary Before the Envelope

```
1. process_event_content receives VerifiedEventContent:
   - Insert its carried verified envelope
   - Establish Missing, Deleted, or Pruned state using normal envelope rules
   - Process eligible content in the same transaction

2. The envelope later arrives separately:
   - Event insertion reports AlreadyPresent
   - RC, usage accounting, scheduling, and projections remain unchanged
```

This path is semantically the same as `process_event_with_content`. If the
carried envelope was already inserted, only eligible Missing content is
processed. Processed, Deleted, Pruned, and Invalid states retain their usual
idempotent behavior.

Latest-event projections apply one order while processing side effects:
follow/unfollow state, profiles, generic singletons, and individual votes keep
the maximum `(event.timestamp, ShortEventId)`. The vote aggregate changes only
when the same candidate wins the individual-vote comparison. Equal-second
events therefore converge independently of payload delivery order.

Replacing an individual-vote winner first validates its complete stored source
relationship: source key/ID, author, kind, singleton and auxiliary-parent flags,
timestamp, verified retained bytes, decoded vote, and target must agree. A
Deleted or Pruned source remains resolvable while those bytes remain because
non-post projections survive those terminal transitions. A Missing, Invalid,
absent, or mismatched source is corruption. Transaction processing fails and
rolls back the whole transaction, including the candidate envelope, without
changing the winner or aggregate; the current infallible public ingestion
boundary then panics on that storage-corruption error.

Active follows also retain the latest unfollow as an exclusive epoch boundary
and retain processed follow-event orders. `first_ts` is the timestamp of the
earliest follow strictly after that boundary, so late follow delivery can move
the current epoch's start earlier without allowing a pre-unfollow follow into a
later epoch. Follow/unfollow events in the same second use `ShortEventId` to
decide epoch membership. Notification cutoff remains timestamp-only: a social
post or shoutbox timestamp below both database initialization and `first_ts`
uses its authored time in the receipt index; one equal to `first_ts` uses local
receipt time.

Deployment staging and the deterministic authored-time fallback for receipt
indexes rebuilt by total migration are recorded in the
[ARCH-client-database status](../specs/ARCH-client-database.md#status).

### Flow 3: Content Deletion Before Target Event Arrives

Parent IDs resolve only within the deleting event's author graph, as specified
by [SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md). A present
event by another author with the same short ID follows this missing-target flow
and cannot be mutated.

```
1. Delete event D arrives, same-author target T not in `events`:
   - T added to `events_missing` with deleted_by = D
   - An ordinary child referencing T preserves deleted_by = D
   - Another direct deleter is merged canonically; the maximum
     `(event.timestamp, ShortEventId)` becomes deleted_by

2. Same-author target event T finally arrives:
   - Check `events_missing`: found with deleted_by
   - Mark T's content as Deleted in `events_content_state`
   - Do NOT increment RC (content already marked for deletion)
   - Account T's payload directly in total and deleted usage without first
     entering Missing
   - Do NOT mark content as Missing
```

Deletion intent is monotone in both `events_missing` and
`events_content_state`. Multiple direct deleting children select the same
canonical `deleted_by` regardless of delivery order. Changing only the winner
does not repeat RC, queue, usage, or projection-reversion work.

Deletion affects content, not the signed header. In the direct chain
`T <-delete- D1 <-delete- D2`, D1's header keeps deleting T even after D1's own
content is deleted. The direct attributions remain `T.deleted_by = D1` and
`D1.deleted_by = D2`; D2 is not transitively assigned to T.

### Flow 4: Content Deletion After Target (While Content Missing)

```
1. Event T arrives in its author's graph:
   - RC = 1
   - T's content = Missing

2. Same-author delete event D arrives targeting T's content:
   - old_state = Missing
   - Set T's content = Deleted
   - Decrement RC (now 0)

3. Content for T arrives:
   - can_insert_event_content_tx: T's content = Deleted → return false
   - Ordinary content processing is skipped
   - A below-limit SOCIAL_POST with the delete-aux flag, an auxiliary
     parent, valid CBOR, and nonempty `djot_content.trim()` may record only its
     immutable forward and reverse replacement rows
```

The same limited replacement extraction applies when the deletion arrived
before the target envelope. In an edit chain `E <- A <- B`, delivery of B can
stage A as Deleted; later verified A content still records `E -> A`, so lookup
of E follows `E -> A -> B`. Blank deletion posts do not record an edit edge.
Missing, empty, or whitespace-only `djot_content` is blank regardless of other
social-post fields. The database also checks already-retained hash-keyed bytes
when a predeleted envelope arrives, but it never schedules or requests Deleted
content. Total replay preserves the immutable forward row and reconstructs the
reverse index without depending on zero-RC payload bytes; already-retained
bytes remain independently GC-eligible. Deleted content never becomes visible
or retrievable and does not update RC, queues, usage, time/reception indexes,
reply/reaction counts, news, mentions, votes, singletons, or notifications.

For an ordinarily processed SOCIAL_POST, insertion and later deletion
reversion use the same projection-applicability classification:

- a post without the delete-aux flag applies ordinary projections;
- a post with the delete-aux flag but no auxiliary parent applies no ordinary
  projections, even when its body is nonblank;
- a deleting post with an auxiliary parent and nonempty trimmed
  `djot_content` is an edit and applies ordinary projections;
- a deleting post with absent, empty, or whitespace-only `djot_content`
  applies no ordinary projections, even when reply, reaction, or news fields
  are populated.

The symmetric rule covers the authored-time, reply, reaction, news, and
self-mention projections, plus the effective-reception forward row and its
event-to-key reverse mapping. The receipt directions are inserted and removed
atomically. Reversion leaves the shared durable reception allocator advanced;
deleted sequence values are never reused. A reverse key whose forward row is
absent or names another event is an invariant failure and rolls back deletion.
Reversion likewise does not compensate with saturating arithmetic: an eligible
projection with inconsistent counters remains an invariant failure. Immutable
replacement lineage follows its separate, Deleted-state exception above.
Unmapped version-24 receipt rows created before this change remain staged as
described by the
[ARCH-client-database status](../specs/ARCH-client-database.md#status).

### Flow 5: Content Deduplication (Multiple Events, Same Hash)

```
1. Event A with hash H: RC(H) = 1, A's content = Missing
2. Event B with hash H: RC(H) = 2, B's content = Missing
3. Content arrives, process A: A side effects, A content processed
4. Process B (same content): B side effects, B content processed
5. Delete A's content: RC(H) = 1, content still available for B
6. Delete B's content: RC(H) = 0, content can be GC'd
```

### Flow 6: Invalid Content

```
1. Event T arrives:
   - RC = 1
   - T's content = Missing

2. Content arrives, process_event_content_tx:
   - Side effects processing fails (e.g. CBOR deserialization error)
   - Set T's content = Invalid in `events_content_state`
   - Decrement RC (now 0)
   - Content bytes NOT stored in `content_store`

3. If author later deletes T's content:
   - old_state = Invalid
   - Set T's content = Deleted (records deletion intent)
   - RC NOT decremented again (already decremented)
```

## Idempotency Guarantee

The `Missing` state ensures content processing is idempotent:

```rust
fn can_insert_event_content_tx(...) -> bool {
    match events_content_state.get(event_id) {
        Some(Missing) => true,       // Process it
        None => false,               // Already processed
        Some(Deleted|Pruned|Invalid) => false, // Unwanted/bad
    }
}
```

This prevents duplicate side effects when:
- Same event is delivered multiple times
- Same content is delivered multiple times for same event

## Edge Cases and Guards

### Double-Decrement Prevention

When deleting/pruning content, we check old_state:

```rust
if !matches!(old_state, Some(Deleted { .. } | Pruned | Invalid)) {
    decrement_rc(...);  // Only if not already decremented
}
```

### Content Delete After Prune/Invalid

- Event content pruned/marked invalid: state = Pruned/Invalid, RC decremented
- Content deletion event arrives: state changes to Deleted, RC NOT decremented again
- Semantic: Author's content deletion intent is recorded, but no double-decrement

### Prune After Content Delete/Invalid

- Event content deleted/marked invalid: state = Deleted/Invalid, RC decremented
- Prune attempted: returns false (content already deleted/invalid)

### Event Already Present

```rust
if events_table.get(&event_id)?.is_some() {
    return Ok(InsertEventOutcome::AlreadyPresent);
}
```

Duplicate event delivery is a no-op. RC not incremented again.

## Content Fetch Scheduling

Missing content is fetched by the `MissingEventContentFetcher` task using an
event-driven approach with exponential backoff.

### Table Structure

The `events_content_missing` table uses a composite key `(Timestamp,
ShortEventId)` where the `Timestamp` is the scheduled next fetch attempt time.
This makes the table naturally sorted by when content should next be fetched.

### Missing State Metadata

The `EventContentState::Missing` variant tracks fetch attempt metadata:

```rust
Missing {
    last_fetch_attempt: Option<Timestamp>,  // when we last tried (fact)
    fetch_attempt_count: u16,               // how many times we tried (fact)
    next_fetch_attempt: Timestamp,          // when to try next (scheduling)
}
```

The `next_fetch_attempt` field mirrors the `Timestamp` component of the current
`events_content_missing` key, enabling removal (which requires the full
composite key). In canonical state, each event has at most one queue row. A row
is current only when the event is `Missing` and its timestamp exactly equals
`next_fetch_attempt`; non-Missing events have no current queue row. A Missing
event whose bytes are already available locally can temporarily have no queue
row while local processing completes. Legacy inconsistent physical rows can
remain behind valid work until lazy front repair or total replay, but queue
APIs filter them from current work.

### Fetcher Loop

Instead of scanning the entire missing table on a fixed interval, the fetcher:

1. Transactionally deletes inconsistent front rows until it finds an exact
   state/schedule match
2. Peeks at that first valid entry (smallest key = earliest due)
3. If due now: attempts to fetch from peers
4. If not due: sleeps until the scheduled time
5. If no valid row remains: waits for a `Notify` signal

A `Notify` channel wakes the fetcher immediately when new missing content is
inserted (via `on_commit` hook in `process_event_tx`).

### Backoff Formula

On fetch failure, the next attempt is scheduled with exponential backoff:

```
backoff_secs = min(60 * 1.5^(attempt_count - 1), 86400)
```

- Initial backoff: 60 seconds (1 minute)
- Maximum backoff: 86400 seconds (24 hours)
- New entries start with `next_fetch_attempt = Timestamp::ZERO` (try immediately)

### Failed Fetch Recording

The `record_failed_content_fetch` DB method:

1. Reads the current `Missing` state and compares its schedule with the
   caller-observed schedule
2. Ignores the completion if the state is no longer Missing or its schedule has
   changed, or if `next_attempt_at` is not strictly later than the current
   schedule
3. Removes the schedule entry mirrored by the current state
4. Inserts one schedule entry with updated `next_attempt_at`
5. Updates `events_content_state` with incremented count and timestamps

The caller provides both `attempted_at` (fact) and `next_attempt_at`
(scheduling decision). The backoff calculation lives in the fetcher, not the
DB layer. This compare-and-set behavior makes overlapping fetch completions
safe: only the completion for the current schedule can advance retry metadata.
Strictly increasing schedules prevent a duplicate completion from succeeding
through reuse of the same schedule value.

Queue peeking repairs legacy or otherwise inconsistent front rows in the same
write transaction used to select work. It removes rows for non-Missing or
absent events and rows whose timestamp differs from `next_fetch_attempt`, then
continues to later work without returning an empty result. A total migration
discards old queue rows and derives the current queue from retained events and
content; there is no separate queue-repair migration.

Queue pagination omits inconsistent rows even when they remain behind a valid
front row awaiting lazy repair, so diagnostic/API consumers see only current
fetch work.

## Potential Concerns

### 1. No Automatic Garbage Collection

When RC reaches 0, content remains in `content_store`. A separate GC process
should periodically clean up content with RC=0. (Future work)

### 2. Missing Events / Missing RC

These abnormal internal conditions are detected and logged:

- **Calling `process_event_content_tx` for a non-existent event**:
  `debug_assert!` + `error!` log, then silently skipped in release mode. The
  public `process_event_content` boundary inserts the carried envelope first.
- **Decrementing RC with no RC entry**: `debug_assert!` + `error!` log, then
  defaults to 1 to avoid underflow.

Both cases indicate bugs in the calling code and will panic in debug builds.

## Test Coverage

### Core Flow Tests

- `test_event_arrives_before_content` - Event before content flow
- `test_content_exists_when_event_arrives` - Content before event flow
- `public_content_ingestion_matches_combined_for_both_arrival_orders` - Public
  content-only, envelope-first, and combined ingestion converge across duplicate
  calls, reopen, and total replay, including Processed state, RC, queue, usage,
  and projections
- `public_content_ingestion_preserves_terminal_states` - Repeated public content
  ingestion preserves Deleted, Pruned, and Invalid lifecycle outcomes
- `test_multiple_events_share_content` - Deduplication + pruning
- `test_multiple_events_waiting_for_content` - Multiple events, same hash
- `test_delete_event_arrives_before_target` - Delete before target
- `test_predeleted_envelope_bookkeeping_converges` - Delete-before-target and
  target-before-delete produce equal self-envelope indexing, payload usage, RC,
  queue, and Deleted state across reopen and total replay
- `deleted_intermediate_lineage_converges_for_all_valid_deliveries` - All 90
  envelope-before-own-payload schedules produce identical transitive edit
  lineage, lifecycle bookkeeping, and semantic visibility
- `supplied_predeleted_edit_changes_only_lineage` and
  `supplied_predeleted_blank_delete_changes_nothing` - Supplied Deleted content
  adds only eligible nonblank edit metadata; blank deletion content is inert
- `retained_deleted_edit_lineage_survives_reopen_gc_and_total_replay` -
  hash-deduplicated retained content derives lineage without payload delivery,
  and immutable forward metadata survives reopen, byte removal, and total replay
- `over_limit_deleted_edit_derives_no_lineage_from_retained_or_supplied_bytes`
  - Both Deleted-content activation paths reject over-limit replacement content
- `test_content_processing_idempotency` - Duplicate content delivery
- `deleting_post_projection_reversion_is_symmetric` - Absent-body, empty, and
  whitespace deleting posts leave zero or unrelated nonzero reply/reaction
  state unchanged, while nonblank edits apply and revert authored-time, reply,
  news, and mention projections across target-first/delete-first delivery,
  duplicates, reopen, and total replay
- `ordinary_reaction_projection_still_reverts` - Eligible reaction projection
  insertion and checked deletion reversion remain active
- `delete_flag_without_aux_is_projection_inert` - A signed delete-aux flag
  without an auxiliary parent remains inert even with nonblank projection fields
- `inconsistent_eligible_reaction_reversion_fails_and_rolls_back` - An eligible
  projection with a corrupt zero aggregate fails checked reversion and rolls
  back deletion lifecycle and index changes, including both receipt directions
- `social_receipt_reversion_updates_cursors_without_reusing_order` - Reverting
  the latest ordinary post removes both receipt directions, moves raw latest and
  pagination cursors to the retained post, and preserves allocator durability
  and collision safety across duplicate deletion and reopen
- `inconsistent_social_receipt_mapping_aborts_complete_deletion` - A reverse key
  whose forward row is absent or resolves to another event fails closed and
  rolls back the complete deletion transaction
- `duplicate_social_receipt_mapping_aborts_complete_insertion` - A preexisting
  reverse mapping fails closed and rolls back the event, projections, and
  allocator while preserving the existing mapping
- `unmapped_v24_receipt_is_staged_until_total_replay` - Reopen initializes the
  reverse table without a version bump; an unmapped legacy row coexists with
  exact new mappings until deterministic total replay removes stale membership

### Edge Case Tests

- `test_delete_while_unprocessed` - Content delete arrives while content is Missing
- `test_two_deletes_same_target` - Second content delete doesn't double-decrement RC
- `test_deletion_is_monotone_across_delivery_permutations` - Ordinary children
  cannot erase staged deletion
- `test_direct_deleters_converge_across_delivery_permutations` - Direct
  attribution uses canonical timestamp/ID precedence
- `test_equal_timestamp_deleters_use_event_id_tiebreak` - Equal-time direct
  attribution is total
- `test_deleter_attribution_update_does_not_repeat_reversion` - Canonical
  attribution updates do not revert processed projections twice
- `test_deletion_chain_preserves_header_effects` - Deletion chains retain direct
  non-transitive attribution, including staged ordinary-child interference
- `test_prune_then_delete` - Content Prune→Delete transition, no double-decrement
- `test_delete_then_prune` - Prune after content delete returns false
- `test_cross_author_parent_never_resolves_or_deletes` - Cross-author raw-ID
  matches stay author-scoped missing in either arrival order
- `test_process_content_for_nonexistent_event` - Silent skip (release only)
- `test_data_usage_payload_invalid` - Invalid content: Missing→Invalid, RC decremented
- `test_data_usage_invalid_payload_deletion` - Deleting invalid: Invalid→Deleted, no RC change
- `test_equal_timestamp_follow_conflicts_converge` - Follow/unfollow and selector
  conflicts converge in both orders
- `follow_epochs_converge_across_zero_one_and_two_unfollows` - Shuffled follow
  histories converge on active selector, current-epoch `first_ts`, and
  historical notification cutoffs
- `equal_second_event_order_defines_epoch_membership` - Same-second follow and
  unfollow events use the event-ID tie-break while the timestamp cutoff treats
  content at `first_ts` as current
- `post_and_shout_notifications_use_current_epoch_cutoff` - Both receipt indexes
  apply the current follow epoch's strict timestamp cutoff
- `metadata_only_epoch_changes_publish_followee_state` - Late nonwinning
  follow/unfollow changes publish the retained `first_ts` projection
- `follow_epoch_survives_reopen_and_total_replay` - Retained state and canonical
  replay derive the same follow epoch
- `test_equal_timestamp_latest_values_converge` - Profile and generic singleton
  conflicts converge in both orders
- `test_equal_timestamp_vote_conflicts_converge` - Vote winner and aggregate use
  one equal-second comparison
- `test_unresolved_vote_winners_abort_and_roll_back` - Missing, mismatched, or
  cryptographically invalid stored vote sources abort replacement without
  changing the winner, aggregate, or candidate envelope across reopen
- `deleted_vote_winner_with_retained_content_can_be_replaced` - Normal
  vote-delete-revote flow replaces a terminal winner while its verified content
  remains retained
- `latest_singleton_query_is_isolated_ordered_and_strict` - Public singleton
  enumeration is identity/kind isolated, deterministically newest-first, and
  rejects malformed stored keys
- `test_failed_fetch_completions_are_compare_and_set` - Overlapping failed
  completions cannot leave stale rows after processing or deletion
- `test_failed_fetch_rejects_non_forward_schedule` - Equal or backward
  replacement schedules cannot reuse a current CAS token
- `test_missing_content_peek_repairs_stale_front_rows` - Fetcher peeking removes
  inconsistent front rows, pagination filters stale tail rows, later valid work
  remains reachable, and repairs persist across reopen
- `test_total_migration` - Total replay discards inconsistent queue rows and
  derives one exact row for retained Missing content

### Property and Shuffled-Order Tests

- [`property-testing.md`](property-testing.md) documents the shared two-replica
  schedule runner, semantic models, exclusions, runtime budget, and soak command
- `prop_author_scoped_event_graph_converges` - Envelope graph indexes and
  unresolved canonical deletion attribution converge
- `prop_live_raw_content_lifecycle_converges` - Live RAW content bytes, RC,
  queue termination, and complete usage accounting converge
- `prop_follow_semantics_converge` - Follow winners, reverse membership,
  canonical epochs, and retained unfollow boundaries converge
- `prop_profile_and_singleton_semantics_converge` - Profile fields and
  profile/generic singleton winners converge
- `prop_vote_semantics_converge` - Per-voter winners and normalized numerical
  vote aggregates converge
- `test_shuffled_singleton_events_converge` - Shuffled finite latest-event sets
  select the total-order maximum

## Summary

The content lifecycle model handles:

- Out-of-order delivery (event before content, content delete before target)
- Duplicate delivery (idempotent via Missing state)
- Content deduplication (RC tracks multiple events per hash)
- Empty content (processed immediately at insertion unless already Deleted)
- Invalid content (failed validation, RC decremented, bytes discarded)
- Content deletion and pruning (with double-decrement prevention)
- Fetch scheduling (exponential backoff for missing content, event-driven wake-up)

The `Missing` state is the key to idempotency - it ensures content side
effects are applied exactly once per event, regardless of how many times the
content is delivered.
