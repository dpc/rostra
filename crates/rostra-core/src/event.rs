pub mod content;
pub mod content_kind;

use std::{fmt, str};

pub use content::*;
pub use content_kind::*;

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "serde")]
mod serde;

#[cfg(feature = "bincode")]
mod bincode;

#[cfg(all(feature = "ed25519-dalek", feature = "bincode"))]
mod verified_event;

#[cfg(all(feature = "ed25519-dalek", feature = "bincode"))]
pub use verified_event::*;

use crate::id::RostraId;
use crate::{
    ContentHash, MsgLen, NullableShortEventId, ShortEventId, Timestamp, TimestampFixed,
    array_type_define, array_type_impl_base64_str, array_type_impl_serde,
    array_type_impl_zero_default,
};

/// Convenience extension trait to unify getting event data from all versions
/// of [`Event`].
pub trait EventExt {
    fn event(&self) -> &Event;

    fn author(&self) -> RostraId {
        self.event().author
    }

    fn flags(&self) -> u8 {
        self.event().flags
    }

    fn aux_key(&self) -> EventAuxKey {
        self.event().key_aux
    }

    fn kind(&self) -> EventKind {
        self.event().kind
    }
    fn timestamp(&self) -> Timestamp {
        self.event().timestamp.into()
    }
    fn content_hash(&self) -> ContentHash {
        self.event().content_hash
    }
    fn parent_prev(&self) -> Option<ShortEventId> {
        self.event().parent_prev.into()
    }
    fn parent_aux(&self) -> Option<ShortEventId> {
        self.event().parent_aux.into()
    }
    fn content_len(&self) -> u32 {
        self.event().content_len.into()
    }
    fn is_delete_parent_aux_content_set(&self) -> bool {
        self.event().is_delete_parent_aux_content_set()
    }

    fn is_singleton(&self) -> bool {
        self.event().is_singleton()
    }
}

/// An event (header), as encoded on the wire
///
/// The smallest building block of Rostra's data model.
///
/// Events chain up to two previous events, forming
/// a DAG that can be traversed from the present to the past.
///
/// Intentionally crafted to be a small and fixed:
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
    /// should consider the **content** of the event from `parent_aux` deleted,
    /// and delete itfrom their storage, recording the id of
    /// this event (one that deleted it) instead. The p2p and other
    /// protocols should accommodate such missing events as a core
    /// feature of the protocol and no longer store or return content data.
    ///
    /// Big `1` - singleton - only the last value of this event for a given
    /// `(kind, key)` really matters, and previous ones can be considered
    /// deleted.
    ///
    /// All other bits MUST be 0 when producing headers, but might
    /// gain meaning in the future, so should still be accepted and
    /// ignored by client that don't understand them.
    pub flags: u8,

    /// The meaning and interpretation of the content from `content_hash`.
    ///
    /// This allows clients to filter events without inspecting their content.
    pub kind: EventKind,

    /// Content length
    ///
    /// Committing to it here, allows clients with storage requirements
    /// to skip data simply too large to bother with.
    ///
    /// Must be valid. Clients will simply reject content that doesn't
    /// match **both** `content_hash` and `content_len`.
    pub content_len: MsgLen,

    /// Timestamp of the message, in a fixed-sized encoding.
    pub timestamp: TimestampFixed,

    /// Key for the `singleton` flag
    ///
    /// Arbitrary bytes that can be used to index the event automatically,
    /// event without inspecting the content.
    pub key_aux: EventAuxKey,

    /// Public id of the creator of the message
    pub author: RostraId,

    /// Previous [`crate::EventId`], to form a kind-of-a-chain (actually DAG).
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
    /// With some `flags` and `kind`s it can point at a past [`Event`]
    /// with special meaning or function, as
    /// opposed to the `parent_event` which is always about exact ordering.
    ///
    /// In all cases used to potentially merge divergent chains
    /// into one DAG. Also, by pointing at some much older `EventId`
    /// it allows fetching older events without traversing
    /// the DAG/chain one by one, potentially suffering latency
    /// of getting the data serially.
    ///
    /// Well behaved clients should try to make it point somewhat
    /// older event, to help it act as a skiplist. A random event
    /// should be easy to implement and recommended.
    ///
    /// `EventID::ZERO` means "None", and is valid, e.g. in
    /// cases where the client does not maintain any history and
    /// relies on forwarding signed events to "active" node.
    ///
    /// It is also OK if `parent_aux == parent_prev`.
    pub parent_aux: NullableShortEventId,

    /// Blake3 hash of the content
    ///
    /// The [`EventContent`] is used to store and interpret
    /// the actual content, and is stored and transmitted outside
    /// of the [`Event`] itself to decouple them.
    pub content_hash: ContentHash,
}

impl Event {
    pub const DELETE_PARENT_AUX_CONTENT_FLAG: u8 = 1;
    pub const SINGLETON_FLAG: u8 = 2;

    pub fn is_delete_parent_aux_content_set(&self) -> bool {
        self.flags & Self::DELETE_PARENT_AUX_CONTENT_FLAG != 0
    }

    pub fn is_singleton(&self) -> bool {
        self.flags & Self::SINGLETON_FLAG != 0
    }
}

impl EventExt for Event {
    fn event(&self) -> &Event {
        self
    }
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct PersonaId(pub u8);

impl PersonaId {
    pub const MIN: Self = Self(0);
    pub const MAX: Self = Self(u8::MAX);
}

impl fmt::Display for PersonaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl str::FromStr for PersonaId {
    type Err = <u8 as str::FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(PersonaId(u8::from_str(s)?))
    }
}
pub trait SignedEventExt: EventExt {
    fn sig(&self) -> EventSignature;
}

/// An [`Event`] along with a [`EventSignature`]
///
/// Notably: not verified yet be any means.
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SignedEvent {
    event: Event,
    sig: EventSignature,
}

impl SignedEvent {
    pub fn unverified(event: Event, sig: EventSignature) -> Self {
        Self { event, sig }
    }

    pub fn sig(&self) -> EventSignature {
        self.sig
    }
}

impl EventExt for SignedEvent {
    fn event(&self) -> &Event {
        &self.event
    }
}

impl SignedEventExt for SignedEvent {
    fn sig(&self) -> EventSignature {
        self.sig
    }
}

impl From<VerifiedEvent> for SignedEvent {
    fn from(event: VerifiedEvent) -> Self {
        Self {
            event: event.event,
            sig: event.sig,
        }
    }
}

array_type_define!(
    #[derive(PartialEq, Eq)]
    struct EventSignature,
    64
);

impl fmt::Display for EventSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        data_encoding::BASE64URL_NOPAD.encode_write(&self.0, f)
    }
}

array_type_define!(
    #[derive(PartialEq, Eq)]
    struct EventAuxKey,
    16
);

array_type_impl_serde!(struct EventAuxKey, 16);
array_type_impl_base64_str!(EventAuxKey);
array_type_impl_zero_default!(EventAuxKey, 16);

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
    /// Control: Node Announcement
    pub const NODE_ANNOUNCEMENT: Self = EventKind::from_u16(0x13);

    /// Social Post, backbone of the social network
    pub const SOCIAL_POST: Self = EventKind::from_u16(0x20);
    pub const SOCIAL_LIKE: Self = EventKind::from_u16(0x21);
    pub const SOCIAL_REPOST: Self = EventKind::from_u16(0x22);
    pub const SOCIAL_PROFILE_UPDATE: Self = EventKind::from_u16(0x24);

    pub const fn from_u16(value: u16) -> Self {
        Self(value.to_be_bytes())
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match *self {
            Self::NULL => "null",
            Self::RAW => "raw",
            Self::FOLLOW => "follow",
            Self::UNFOLLOW => "unfollow",
            Self::PERSONA_UPDATE => "persona-update",
            Self::NODE_ANNOUNCEMENT => "node-announcement",
            Self::SOCIAL_POST => "social-post",
            Self::SOCIAL_LIKE => "social-like",
            Self::SOCIAL_REPOST => "social-repost",
            Self::SOCIAL_PROFILE_UPDATE => "social-profile-update",
            v => {
                f.write_fmt(format_args!("{v}"))?;
                return Ok(());
            }
        };

        f.write_str(s)
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
