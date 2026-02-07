//! Event-related database record types.
//!
//! Events in Rostra form a DAG (Directed Acyclic Graph) where each event
//! references parent events. The event "envelope" (metadata + signature) is
//! stored separately from the event "content" (payload) to allow content
//! pruning while maintaining DAG integrity.

use std::borrow::Cow;

use bincode::{Decode, Encode};
use rostra_core::ShortEventId;
use rostra_core::event::{EventContentRaw, EventContentUnsized, EventExt, SignedEvent};
use serde::Serialize;

/// Record for the main `events` table.
///
/// Contains the signed event envelope (metadata + signature). The actual
/// content is stored separately in `events_content`.
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub struct EventRecord {
    /// The signed event (includes event metadata and cryptographic signature)
    pub signed: SignedEvent,
}

impl EventExt for EventRecord {
    fn event(&self) -> &rostra_core::event::Event {
        self.signed.event()
    }
}

/// Record for the `events_missing` table.
///
/// When we receive an event that references a parent we don't have, we create
/// a "missing" record for that parent. This drives sync to fetch the missing
/// event.
#[derive(Decode, Encode, Debug)]
pub struct EventsMissingRecord {
    /// If set, a deletion event was received before the actual event.
    ///
    /// When the missing event is finally received, it should be marked as
    /// deleted immediately rather than processed normally.
    ///
    /// Note: We only store one `deleted_by` ID. If multiple deletion events
    /// target the same missing event, we arbitrarily keep one.
    pub deleted_by: Option<ShortEventId>,
}

/// Marker record for the `events_heads` table.
///
/// The key `(author, event_id)` identifies the head; this empty struct just
/// marks its existence. A "head" is an event with no known children - the
/// current tip of the DAG for that author.
#[derive(Decode, Encode, Debug)]
pub struct EventsHeadsTableRecord;

/// Record for singleton event tables (`events_singletons`,
/// `events_singletons_new`).
///
/// For singleton event types (where only the latest matters), we just need to
/// track the event ID; the rest can be looked up in the main `events` table.
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub struct EventSingletonRecord {
    /// The event ID of the latest singleton event
    pub event_id: ShortEventId,
}

// ============================================================================
// Content Deduplication Types (V1)
//
// These types support storing content by hash for deduplication across events.
// ============================================================================

/// Content stored in the `content_store` table, keyed by ContentHash.
///
/// This enables content deduplication - identical content (e.g., the same
/// image posted by multiple users) is stored only once.
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub enum ContentStoreRecord<'a> {
    /// Content is present and valid
    Present(Cow<'a, EventContentUnsized>),
    /// Content is present but was invalid during processing.
    ///
    /// We keep invalid content so we don't try to fetch it again, but we
    /// don't process its effects.
    Invalid(Cow<'a, EventContentUnsized>),
}

/// Owned version of [`ContentStoreRecord`].
pub type ContentStoreRecordOwned = ContentStoreRecord<'static>;

/// Per-event content processing state stored in `events_content_state` table.
///
/// This enum tracks the content processing lifecycle for each event:
///
/// ## State Transitions
///
/// ```text
/// Event inserted ──► Unprocessed ──► (no entry) ──► Deleted/Pruned
///                         │               │
///                         │               └─ Content processed successfully
///                         │                  (side effects applied)
///                         │
///                         └─ Can also go directly to Deleted/Pruned
/// ```
///
/// ## Interpretation
///
/// - **No entry in table**: Content has been processed for this event. This is
///   the normal state after successful content processing. Side effects (reply
///   counts, follow updates, etc.) have been applied.
///
/// - **`Unprocessed`**: Event was inserted but content processing hasn't
///   happened yet. The event's content side effects have NOT been applied.
///
/// - **`Deleted` / `Pruned`**: Content is unwanted. RC has been decremented.
///
/// ## Idempotency Guarantee
///
/// The `Unprocessed` state ensures content processing is idempotent:
/// - `process_event_content_tx` checks for `Unprocessed` before processing
/// - If `Unprocessed` → process content, then remove the marker
/// - If no entry → skip (already processed)
/// - If `Deleted`/`Pruned` → skip (content unwanted)
///
/// This prevents duplicate side effects (e.g., incrementing reply_count twice)
/// when the same content is received multiple times for the same event.
#[derive(Debug, Encode, Decode, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum EventContentState {
    /// Content has not been processed yet for this event.
    ///
    /// This state is set when an event is inserted (in `insert_event_tx`).
    /// Content side effects have NOT been applied yet.
    ///
    /// When `process_event_content_tx` runs and sees this state:
    /// 1. It processes the content (applies side effects)
    /// 2. Stores content in `content_store` if not already there
    /// 3. Removes this `Unprocessed` marker (deletes entry from table)
    ///
    /// After processing, the event will have NO entry in
    /// `events_content_state`, indicating content was successfully
    /// processed.
    Unprocessed,

    /// Content was deleted by the author via a deletion event.
    ///
    /// When a deletion event targets this event's content:
    /// - This state is set with a reference to the deleting event
    /// - RC is decremented in `content_rc`
    /// - Content may be garbage collected if RC reaches 0
    Deleted {
        /// The event that requested this content be deleted
        deleted_by: ShortEventId,
    },

    /// Content was pruned locally (e.g., too large to store).
    ///
    /// Unlike `Deleted`, this is a local decision, not an author request.
    /// Used when content exceeds size limits or for storage management.
    Pruned,
}

/// Result of looking up event content.
///
/// This is returned by `get_event_content_full_tx` and combines the per-event
/// state with the actual content from the content store.
#[derive(Debug, Clone)]
pub enum EventContentResult {
    /// Content is present and valid
    Present(EventContentRaw),
    /// Content is present but was invalid during processing
    Invalid(EventContentRaw),
    /// Content was deleted by the author
    Deleted {
        /// The event that requested this content be deleted
        deleted_by: ShortEventId,
    },
    /// Content was pruned locally
    Pruned,
    /// Content is not in the store (check events_content_missing)
    Missing,
}

impl EventContentResult {
    /// Returns the content if present (either valid or invalid).
    pub fn content(&self) -> Option<&EventContentRaw> {
        match self {
            EventContentResult::Present(c) | EventContentResult::Invalid(c) => Some(c),
            _ => None,
        }
    }

    /// Returns true if content is present and valid.
    pub fn is_present(&self) -> bool {
        matches!(self, EventContentResult::Present(_))
    }

    /// Returns true if content was deleted.
    pub fn is_deleted(&self) -> bool {
        matches!(self, EventContentResult::Deleted { .. })
    }
}

// ============================================================================
// Event Reception Tracking (V6)
//
// These types track when and how we received each event.
// ============================================================================

use rostra_core::event::IrohNodeId;
use rostra_core::id::RostraId;

/// How we acquired an event.
///
/// Tracks the source and method by which we received an event, useful for
/// debugging sync issues, understanding network propagation, and analytics.
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub enum EventReceivedSource {
    /// Event was backfilled during a database migration.
    ///
    /// For events that existed before we started tracking reception info.
    Migration,

    /// Event was pushed to us by a peer.
    Pushed {
        /// The identity that sent us this event (if known).
        from_id: Option<RostraId>,
        /// The iroh node that sent us this event (if known).
        from_node: Option<IrohNodeId>,
    },

    /// We actively pulled/requested this event.
    Pulled {
        /// The identity we pulled from (if known).
        from_id: Option<RostraId>,
        /// The iroh node we pulled from (if known).
        from_node: Option<IrohNodeId>,
        /// Description of the task/query that triggered this pull.
        task: Option<String>,
    },

    /// Event was created locally by this node.
    Local,
}

/// Information about when and how we received an event.
///
/// Stored in the `events_received_at` table, keyed by `(Timestamp,
/// reception_order)`.
///
/// The key `(Timestamp, u64)` is guaranteed unique by the monotonic
/// reception_order counter. The event_id is stored in the value to allow
/// lookups and assertions.
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub struct EventReceivedRecord {
    /// The event that was received.
    pub event_id: ShortEventId,
    /// How we acquired this event.
    pub source: EventReceivedSource,
}
