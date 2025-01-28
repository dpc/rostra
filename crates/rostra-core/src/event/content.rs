use std::borrow::Borrow;
use std::ops;
use std::sync::Arc;

#[derive(Debug)]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode))]
#[repr(transparent)]
pub struct EventContentUnsized([u8]);

impl EventContentUnsized {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl ToOwned for EventContentUnsized {
    type Owned = EventContent;

    fn to_owned(&self) -> Self::Owned {
        EventContent(self.0.into())
    }
}

impl AsRef<[u8]> for EventContentUnsized {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
/// Content associated with the event before deserializing
///
/// Before semantic meaning of event is processed, it's content is just a bunch
/// of bytes.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
pub struct EventContent(
    /// We never modify the content, while it is hard to avoid ever cloning it,
    /// so let's make cloning cheap
    Arc<[u8]>,
);

impl ops::Deref for EventContent {
    type Target = EventContentUnsized;

    fn deref(&self) -> &Self::Target {
        self.borrow()
    }
}

impl EventContent {
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

impl From<Arc<[u8]>> for EventContent {
    fn from(value: Arc<[u8]>) -> Self {
        EventContent(value)
    }
}

impl From<Vec<u8>> for EventContent {
    fn from(value: Vec<u8>) -> Self {
        EventContent(value.into())
    }
}

impl AsRef<[u8]> for EventContent {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<EventContentUnsized> for EventContent {
    #[allow(clippy::needless_lifetimes)]
    fn borrow<'s>(&'s self) -> &'s EventContentUnsized {
        // Safety: [`EventContentUnsized`] is a `repr(transparent)`
        let ptr = &*self.0 as *const [u8] as *const EventContentUnsized;
        unsafe { &*ptr }
    }
}
