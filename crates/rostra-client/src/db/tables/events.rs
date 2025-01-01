use bincode::{Decode, Encode};
use rostra_core::event::{EventContent, SignedEvent};

#[derive(Debug, Encode, Decode, Clone)]
pub enum ContentState {
    Missing,
    Present(EventContent),
    Deleted,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct EventRecord {
    pub event: SignedEvent,
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
