use std::borrow::Cow;

use bincode::{Decode, Encode};
use rostra_core::ShortEventId;
use rostra_core::event::{EventContentUnsized, EventExt, SignedEvent};
use serde::Serialize;

#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub enum EventContentState<'a> {
    /// The event content is present and we processed it without problems
    Present(Cow<'a, EventContentUnsized>),
    /// Deleted as requested by the author
    Deleted {
        // Notably: we only store one ShortEventId somewhat opportunistically
        // If multiple events pointed at the same parent event to be deleted,
        // we might return any.
        deleted_by: ShortEventId,
    },
    Pruned,

    /// The event content is present, but turned out invalid during internal
    /// processing
    ///
    /// The main different is that we are not going to try to revert it when
    /// it's being deleted.
    Invalid(Cow<'a, EventContentUnsized>),
}

pub type EventContentStateOwned = EventContentState<'static>;

#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub struct EventRecord {
    pub signed: SignedEvent,
}

impl EventExt for EventRecord {
    fn event(&self) -> &rostra_core::event::Event {
        self.signed.event()
    }
}

#[derive(Decode, Encode, Debug)]
pub struct EventsMissingRecord {
    // Notably: we only store one ShortEventId somewhat opportunistically
    // If multiple events pointed at the same parent event to be deleted,
    // we might return any.
    pub deleted_by: Option<ShortEventId>,
}

#[derive(Decode, Encode, Debug)]
pub struct EventsHeadsTableRecord;
