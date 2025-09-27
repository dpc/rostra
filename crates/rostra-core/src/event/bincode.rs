use std::time::{SystemTime, UNIX_EPOCH};

use convi::ExpectInto as _;

use super::{
    Event, EventAuxKey, EventContentKind, EventContentRaw, EventContentUnsized, EventKind,
    SignedEvent,
};
use crate::bincode::STD_BINCODE_CONFIG;
use crate::id::RostraId;
use crate::{ContentHash, EventId, MsgLen, ShortEventId};

impl EventContentRaw {
    pub fn compute_content_hash(&self) -> ContentHash {
        blake3::hash(self.as_ref()).into()
    }
}

#[bon::bon]
impl Event {
    #[builder(start_fn = builder_raw_content, finish_fn = build)]
    pub fn new_raw_content(
        author: RostraId,
        kind: impl Into<EventKind>,
        delete: Option<ShortEventId>,
        singleton_aux_key: Option<EventAuxKey>,
        parent_prev: Option<ShortEventId>,
        parent_aux: Option<ShortEventId>,
        timestamp: Option<SystemTime>,
        content: Option<&EventContentRaw>,
    ) -> Self {
        if delete.is_some() && parent_aux.is_some() {
            panic!("Can't set both both delete and parent_aux");
        }

        let replace = delete.map(Into::into);
        let parent_prev = parent_prev.map(Into::into);
        let parent_aux = parent_aux.map(Into::into);

        let timestamp = timestamp
            .unwrap_or_else(SystemTime::now)
            .duration_since(UNIX_EPOCH)
            .expect("Dates before Unix epoch are unsupported")
            .as_secs();

        Self {
            version: 0,
            flags: if replace.is_some() { 1 } else { 0 }
                | if singleton_aux_key.is_some() { 2 } else { 0 },
            kind: kind.into(),
            content_len: content
                .as_ref()
                .map(|content| MsgLen(content.len().expect_into()))
                .unwrap_or_default(),
            key_aux: singleton_aux_key.unwrap_or_default(),
            timestamp: timestamp.into(),
            author,
            parent_prev: parent_prev.unwrap_or_default(),
            parent_aux: parent_aux.or(replace).unwrap_or_default(),
            content_hash: content
                .as_ref()
                .map(|content| content.compute_content_hash())
                .unwrap_or(ContentHash::ZERO),
        }
    }

    #[builder(start_fn = builder)]
    pub fn new<C>(
        #[builder(start_fn)] content: &C,
        author: RostraId,
        delete: Option<ShortEventId>,
        parent_prev: Option<ShortEventId>,
        parent_aux: Option<ShortEventId>,
        timestamp: Option<SystemTime>,
    ) -> (Self, EventContentRaw)
    where
        C: EventContentKind,
    {
        let content_raw = content
            .serialize_cbor()
            .expect("Event can't fail to serialize");

        (
            Self::new_raw_content(
                author,
                C::KIND,
                delete,
                content.singleton_key_aux(),
                parent_prev,
                parent_aux,
                timestamp,
                Some(&content_raw),
            ),
            content_raw,
        )
    }

    pub fn compute_id(&self) -> EventId {
        let encoded =
            ::bincode::encode_to_vec(self, STD_BINCODE_CONFIG).expect("Can't fail encoding");
        blake3::hash(&encoded).into()
    }

    pub fn compute_short_id(&self) -> ShortEventId {
        self.compute_id().into()
    }
}

impl SignedEvent {
    pub fn compute_id(&self) -> EventId {
        self.event.compute_id()
    }
    pub fn compute_short_id(&self) -> ShortEventId {
        self.event.compute_id().into()
    }
}

impl<'a, 'de: 'a, C> bincode::BorrowDecode<'de, C> for &'a EventContentUnsized {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let bytes_ref: &[u8] = bincode::BorrowDecode::borrow_decode(decoder)?;
        let ptr = bytes_ref as *const [u8] as *const EventContentUnsized;
        Ok(unsafe { &*ptr })
    }
}

#[cfg(test)]
mod tests;
