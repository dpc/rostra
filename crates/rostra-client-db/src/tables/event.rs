//! Event-related database record types.
//!
//! Events in Rostra form a DAG (Directed Acyclic Graph) where each event
//! references parent events. The event "envelope" (metadata + signature) is
//! stored separately from the event "content" (payload) to allow content
//! pruning while maintaining DAG integrity.

use std::borrow::Cow;

use bincode::{Decode, Encode};
use rostra_core::ShortEventId;
use rostra_core::event::{EventContentUnsized, EventExt, SignedEvent};
use serde::Serialize;

/// The state of an event's content in the database.
///
/// Content is stored separately from event metadata to allow pruning content
/// while keeping the DAG structure intact. This enum tracks the various states
/// content can be in.
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub enum EventContentState<'a> {
    /// Content is present and was successfully processed.
    Present(Cow<'a, EventContentUnsized>),

    /// Content was deleted by the author (via a deletion event).
    ///
    /// Note: We only store one `deleted_by` ID. If multiple events requested
    /// deletion of the same content, we arbitrarily keep one.
    Deleted {
        /// The event that requested this content be deleted
        deleted_by: ShortEventId,
    },

    /// Content was pruned (removed to save space, e.g., for oversized content).
    ///
    /// Unlike `Deleted`, this is a local decision, not an author request.
    Pruned,

    /// Content is present but was invalid during processing.
    ///
    /// We keep invalid content stored so we don't try to fetch it again, but
    /// we don't process its effects. Unlike `Present`, we won't try to revert
    /// its effects if it's later deleted.
    Invalid(Cow<'a, EventContentUnsized>),
}

/// Owned version of [`EventContentState`] (no borrowed data).
pub type EventContentStateOwned = EventContentState<'static>;

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
