use bincode::{Decode, Encode};
use rostra_core::event::{Event, EventSignature};

#[derive(Debug, Encode, Decode, Clone)]
pub enum ContentState {
    Missing,
    Present(Vec<u8>),
    Deleted,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct EventRecord {
    pub event: Event,
    pub signature: EventSignature,
    pub content: ContentState,
}
