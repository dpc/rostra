use std::borrow::Borrow;
use std::ops;
use std::sync::Arc;
#[cfg(feature = "serde")]
mod serde;

#[derive(Debug)]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode))]
#[repr(transparent)]
pub struct EventContentUnsized([u8]);

impl std::fmt::Display for EventContentUnsized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        data_encoding::HEXLOWER.encode_write(self.as_slice(), f)
    }
}
impl EventContentUnsized {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl ToOwned for EventContentUnsized {
    type Owned = EventContentRaw;

    fn to_owned(&self) -> Self::Owned {
        EventContentRaw(self.0.into())
    }
}

impl AsRef<[u8]> for EventContentUnsized {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Content associated with the event before deserializing
///
/// Before semantic meaning of an event is processed, it's content is just a
/// bunch of bytes.
///
/// Look for [`super::content_kind::EventContentKind`]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
pub struct EventContentRaw(
    /// We never modify the content, while it is hard to avoid ever cloning it,
    /// so let's make cloning cheap
    Arc<[u8]>,
);

impl ops::Deref for EventContentRaw {
    type Target = EventContentUnsized;

    fn deref(&self) -> &Self::Target {
        self.borrow()
    }
}

impl EventContentRaw {
    pub fn new(v: Vec<u8>) -> Self {
        Self(v.into())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Arc<[u8]>> for EventContentRaw {
    fn from(value: Arc<[u8]>) -> Self {
        EventContentRaw(value)
    }
}

impl From<Vec<u8>> for EventContentRaw {
    fn from(value: Vec<u8>) -> Self {
        EventContentRaw(value.into())
    }
}

impl AsRef<[u8]> for EventContentRaw {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<EventContentUnsized> for EventContentRaw {
    #[allow(clippy::needless_lifetimes)]
    fn borrow<'s>(&'s self) -> &'s EventContentUnsized {
        // Safety: [`EventContentUnsized`] is a `repr(transparent)`
        let ptr = &*self.0 as *const [u8] as *const EventContentUnsized;
        unsafe { &*ptr }
    }
}
