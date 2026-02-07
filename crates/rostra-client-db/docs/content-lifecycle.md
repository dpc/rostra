# Event Content Lifecycle

> **See also**: `src/tables.rs` for table definitions and inline documentation.
> When updating this document, ensure `tables.rs` stays in sync.

This document describes how events and their content are tracked, processed, and
reference-counted in the Rostra client database.

## Overview

Events in Rostra form a DAG (Directed Acyclic Graph). Each event has an
"envelope" (metadata + signature) and optional "content" (payload). These are
stored separately to enable:

1. **Content deduplication**: Same content shared by multiple events is stored once
2. **Content pruning**: Large content can be discarded while keeping DAG structure
3. **Out-of-order delivery**: Events can arrive before their content

## Key Tables

| Table | Key | Purpose |
|-------|-----|---------|
| `events` | `ShortEventId` | Main event storage (envelope only) |
| `content_store` | `ContentHash` | Content storage (deduplicated by hash) |
| `content_rc` | `ContentHash` | Reference count per content hash |
| `events_content_state` | `ShortEventId` | Per-event processing state |
| `events_content_missing` | `ShortEventId` | Events waiting for content bytes |

## State Machine

Each event's content processing goes through these states:

```
                                ┌─────────────────────────────────┐
                                │                                 │
                                v                                 │
Event Inserted ──► Unprocessed ──► (no entry) ──► Deleted/Pruned │
                        │               │              │          │
                        │               │              └──────────┘
                        │               │              (delete after prune
                        │               │               changes state, no
                        │               │               RC change)
                        └───────────────┴─► Deleted/Pruned
                          (delete/prune      (if deleted before
                           before content     event arrives via
                           processing)        events_missing)
```

### State Meanings

| State | `events_content_state` | Meaning |
|-------|------------------------|---------|
| Unprocessed | `Unprocessed` | Event inserted, content not yet processed |
| Processed | *no entry* | Content processed, side effects applied |
| Deleted | `Deleted { deleted_by }` | Author deleted this content |
| Pruned | `Pruned` | Locally pruned (too large, etc.) |

**Key insight**: "No entry" in `events_content_state` means content was
successfully processed. This is the normal state for most events.

## Reference Counting

RC tracks how many events want a particular content hash. This enables garbage
collection when no events need the content.

### RC Rules

1. **Increment**: When event is inserted (unless already deleted)
2. **Decrement**: When event is deleted or pruned
3. **Never double-decrement**: Guards check if already Deleted/Pruned

### RC and Content Store

- RC is managed at **event insertion time**, not when content arrives
- Content is stored in `content_store` when first processed
- When RC reaches 0, content *can* be garbage collected (not automatic)

## Detailed Flows

### Flow 1: Normal Event Arrival (with content)

```
1. insert_event_tx:
   - Add event to `events`
   - Increment RC for content_hash
   - Mark as Unprocessed in `events_content_state`
   - Content already in store? Skip adding to `events_content_missing`

2. process_event_content_tx:
   - Check can_insert_event_content_tx: Unprocessed? → proceed
   - Apply side effects (reply counts, follow updates, etc.)
   - Store content in `content_store` (if not already there)
   - Remove Unprocessed marker from `events_content_state`
```

### Flow 2: Event Arrives Before Content

```
1. insert_event_tx (event only):
   - Add event to `events`
   - Increment RC
   - Mark as Unprocessed
   - Add to `events_content_missing` (content not in store)

2. Later, content arrives via another event:
   - Content stored in `content_store`

3. process_event_content_tx:
   - Check Unprocessed → proceed
   - Apply side effects
   - Remove from `events_content_missing`
   - Remove Unprocessed marker
```

### Flow 3: Delete Before Target Arrives

```
1. Delete event D arrives, target T not in `events`:
   - T added to `events_missing` with deleted_by = D

2. Target event T finally arrives:
   - Check `events_missing`: found with deleted_by
   - Mark T as Deleted in `events_content_state`
   - Do NOT increment RC (is_deleted = true)
   - Do NOT mark as Unprocessed
```

### Flow 4: Delete After Target (While Unprocessed)

```
1. Event T arrives:
   - RC = 1
   - T = Unprocessed

2. Delete event D arrives targeting T:
   - old_state = Unprocessed
   - Set T = Deleted
   - Decrement RC (now 0)

3. Content for T arrives:
   - can_insert_event_content_tx: T = Deleted → return false
   - Processing skipped
```

### Flow 5: Content Deduplication (Multiple Events, Same Hash)

```
1. Event A with hash H: RC(H) = 1, A = Unprocessed
2. Event B with hash H: RC(H) = 2, B = Unprocessed
3. Content arrives, process A: A side effects, A processed
4. Process B (same content): B side effects, B processed
5. Delete A: RC(H) = 1, content still available for B
6. Delete B: RC(H) = 0, content can be GC'd
```

## Idempotency Guarantee

The `Unprocessed` state ensures content processing is idempotent:

```rust
fn can_insert_event_content_tx(...) -> bool {
    match events_content_state.get(event_id) {
        Some(Unprocessed) => true,   // Process it
        None => false,               // Already processed
        Some(Deleted|Pruned) => false, // Unwanted
    }
}
```

This prevents duplicate side effects when:
- Same event is delivered multiple times
- Same content is delivered multiple times for same event

## Edge Cases and Guards

### Double-Decrement Prevention

When deleting/pruning, we check old_state:

```rust
if !matches!(old_state, Some(Deleted { .. } | Pruned)) {
    decrement_rc(...);  // Only if not already decremented
}
```

### Delete After Prune

- Event pruned: state = Pruned, RC decremented
- Delete arrives: state changes to Deleted, RC NOT decremented again
- Semantic: Author's deletion intent is recorded, but no double-decrement

### Prune After Delete

- Event deleted: state = Deleted, RC decremented
- Prune attempted: returns false (already deleted)

### Event Already Present

```rust
if events_table.get(&event_id)?.is_some() {
    return Ok(InsertEventOutcome::AlreadyPresent);
}
```

Duplicate event delivery is a no-op. RC not incremented again.

## Potential Concerns

### 1. No Automatic Garbage Collection

When RC reaches 0, content remains in `content_store`. A separate GC process
should periodically clean up content with RC=0. (Future work)

### 2. Missing Events / Missing RC

These abnormal conditions are detected and logged:

- **Processing content for non-existent event**: `debug_assert!` + `error!` log,
  then silently skipped in release mode.
- **Decrementing RC with no RC entry**: `debug_assert!` + `error!` log, then
  defaults to 1 to avoid underflow.

Both cases indicate bugs in the calling code and will panic in debug builds.

## Test Coverage

### Core Flow Tests

- `test_event_arrives_before_content` - Event before content flow
- `test_content_exists_when_event_arrives` - Content before event flow
- `test_multiple_events_share_content` - Deduplication + pruning
- `test_multiple_events_waiting_for_content` - Multiple events, same hash
- `test_delete_event_arrives_before_target` - Delete before target
- `test_content_processing_idempotency` - Duplicate content delivery

### Edge Case Tests

- `test_delete_while_unprocessed` - Delete arrives while event is Unprocessed
- `test_two_deletes_same_target` - Second delete doesn't double-decrement RC
- `test_prune_then_delete` - Prune→Delete transition, no double-decrement
- `test_delete_then_prune` - Prune after delete returns false
- `test_process_content_for_nonexistent_event` - Silent skip (release only)

### Property-Based Tests

- `proptest_rc_counting` - Randomized RC correctness
- `proptest_follow_unfollow_delivery_order` - Follow/unfollow ordering

## Summary

The content lifecycle model handles:

- ✅ Out-of-order delivery (event before content, delete before target)
- ✅ Duplicate delivery (idempotent via Unprocessed state)
- ✅ Content deduplication (RC tracks multiple events per hash)
- ✅ Deletion and pruning (with double-decrement prevention)

The `Unprocessed` state is the key to idempotency - it ensures content side
effects are applied exactly once per event, regardless of how many times the
content is delivered.
