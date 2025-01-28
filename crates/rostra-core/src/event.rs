pub mod content_kind;

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "serde")]
mod serde;

#[cfg(feature = "bincode")]
mod bincode;

#[cfg(all(feature = "ed25519-dalek", feature = "bincode"))]
mod verified_event;
use std::borrow::Borrow;
use std::ops;
use std::sync::Arc;

#[cfg(all(feature = "ed25519-dalek", feature = "bincode"))]
pub use verified_event::*;

use crate::id::RostraId;
use crate::{
    define_array_type_no_serde, ContentHash, MsgLen, NullableShortEventId, TimestampFixed,
};

/// An even (header)
///
/// Intentionally crafted to be:
///
/// * version + flags + kind = 1 + 1 + 2 = 4
/// * content_len = 4
/// * timestamp = 8
/// * padding = 16
/// * author = 32
/// * parent * 2 = 16 * 2 = 32
/// * content_hash = 32
///
/// * signature = 64
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Event {
    /// Simple version counter to allow upgrades of [`Event`] format in the
    /// future.
    ///
    /// For now should always be `0`. Any newer version should be rejected.
    pub version: u8,

    /// Bit flags
    ///
    /// Bit `0` - delete previous event - All well-behaved nodes
    /// should consider the event from `parent_aux` deleted, and
    /// delete its content from their storage and record the id of
    /// this event (one that deleted it) instead. The p2p and other
    /// protocols should accommodate such missing events as a core
    /// feature of the protocol and no longer store or return content data.
    ///
    /// All other bits MUST be 0 when producing headers, but might
    /// gain meaning in the future, so should still be accepted and
    /// ignored by client that don't understand them.
    pub flags: u8,

    /// The meaning and interpretation of the content from `content_hash`.
    ///
    /// This allows clients to filter and download data that they need.
    pub kind: EventKind,

    /// Content length
    ///
    /// Committing to it here, allows clients with storage requirements
    /// to skip data simply too large to bother with.
    pub content_len: MsgLen,

    /// Timestamp of the message
    pub timestamp: TimestampFixed,

    /// Unused, reserved for future use, timestamp maybe? (8B)
    pub padding: [u8; 16],

    /// Public id of the creator of the message
    pub author: RostraId,

    /// Previous EventId, to form a kind-of-a-chain (actually DAG).
    ///
    /// It is supposed to be the *latest* `EventID` known to the client
    /// to allow traversing events (almost) in order.
    ///
    /// `EventID::ZERO` means "None" which means that there is
    /// no parent (first event ever), or the node that produced the event
    /// was not capable of knowing it. In such a case it is a job
    /// of the "active" node to connect it to the chain/DAG.
    pub parent_prev: NullableShortEventId,

    /// Auxiliary parent
    ///
    /// With some `flags` and `kind`s it is meant to point at the `EventId`
    /// used for replacement, attachment, or some other special meaning, as
    /// opposed to the `parent_event` which is always about exact ordering.
    ///
    /// In all cases used to potentially merge divergent chains
    /// into one DAG. Also, by pointing at some much older `EventId`
    /// it allows fetching older events, without traversing
    /// the DAG/chain one by one, potentially suffering latency
    /// of getting the data serially.
    ///
    /// Well behaved clients should try to make it point somewhat
    /// older event, to help it act as a skiplist. A random event
    /// might be easy to implement.
    ///
    /// `EventID::ZERO` means "None", and is valid, e.g. in
    /// cases where the client does not maintain any history and
    /// relies on forwarding signed events to "active" node.
    ///
    /// It is also OK if `parent_aux == parent_prev`.
    pub parent_aux: NullableShortEventId,

    /// Blake3 hash of the content
    pub content_hash: ContentHash,
}

impl Event {
    pub const DELETE_PARENT_AUX_CONTENT_FLAG: u8 = 1;

    pub fn is_delete_parent_aux_content_set(&self) -> bool {
        self.flags & Self::DELETE_PARENT_AUX_CONTENT_FLAG != 0
    }
}

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

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PersonaId(pub u32);

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SignedEvent {
    pub event: Event,
    pub sig: EventSignature,
}

define_array_type_no_serde!(
    #[derive(PartialEq, Eq)]
    struct EventSignature,
    64
);

#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EventKind([u8; 2]);

impl EventKind {
    /// No real content
    pub const NULL: Self = EventKind::from_u16(0);
    /// Unspecified binary data
    pub const RAW: Self = EventKind::from_u16(1);

    /// Control: Start following identity
    pub const FOLLOW: Self = EventKind::from_u16(0x10);
    /// Control: Stop following identity
    pub const UNFOLLOW: Self = EventKind::from_u16(0x11);
    /// Control: Persona update
    pub const PERSONA_UPDATE: Self = EventKind::from_u16(0x12);

    /// Social Post, backbone of the social network
    pub const SOCIAL_POST: Self = EventKind::from_u16(0x20);
    pub const SOCIAL_LIKE: Self = EventKind::from_u16(0x21);
    pub const SOCIAL_REPOST: Self = EventKind::from_u16(0x22);
    pub const SOCIAL_COMMENT: Self = EventKind::from_u16(0x23);
    pub const SOCIAL_PROFILE_UPDATE: Self = EventKind::from_u16(0x24);

    pub const fn from_u16(value: u16) -> Self {
        Self(value.to_be_bytes())
    }
}
impl From<u16> for EventKind {
    fn from(value: u16) -> Self {
        Self(value.to_be_bytes())
    }
}

impl From<EventKind> for u16 {
    fn from(value: EventKind) -> Self {
        u16::from_be_bytes(value.0)
    }
}
