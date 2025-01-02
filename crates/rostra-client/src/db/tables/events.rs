use bincode::{Decode, Encode};
use rostra_core::event::{EventContent, SignedEvent};
use rostra_core::ShortEventId;

#[derive(Debug, Encode, Decode, Clone)]
pub enum ContentState {
    Missing,
    Present(EventContent),
    Deleted,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct EventRecord {
    pub event: SignedEvent,
    // Notably: we only store one ShortEventId somewhat opportunistically
    // If multiple events pointed at the same parent event to be deleted,
    // we might return any.
    pub deleted_by: Option<ShortEventId>,
    pub content: ContentState,
}

impl From<Option<EventContent>> for ContentState {
    fn from(value: Option<EventContent>) -> Self {
        match value {
            Some(c) => ContentState::Present(c),
            None => ContentState::Missing,
        }
    }
}

#[derive(Decode, Encode, Debug)]
pub struct EventsMissingRecord {
    // Notably: we only store one ShortEventId somewhat opportunistically
    // If multiple events pointed at the same parent event to be deleted,
    // we might return any.
    pub deleted: Option<ShortEventId>,
}
