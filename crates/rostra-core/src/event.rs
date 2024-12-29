#[cfg(feature = "ed25519-dalek")]
mod ed25519;

use std::collections::BTreeSet;

use crate::id::{RostraId, ShortRostraId};
use crate::{ContentHash, MsgLen, ShortEventId};

/// An even (header)
///
/// Intentionally crafted to be:
///
/// * version + flags + kind = 1 + 1 + 2 = 4
/// * content_len = 4
/// * padding = 8
/// * author = 16
/// * parent * 2 = 16 * 2 = 32
/// * content_hash = 32
///
/// * signature = 64
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Event {
    /// Simple version counter to allow upgrades of [`Event`] format in the
    /// future.
    ///
    /// For now should always be `0`. Any newer version should be rejected.
    pub version: u8,

    /// Bit flags
    ///
    /// Bit `0` - replace/cancel event - All well-behaved nodes
    /// should consider the event from `parent_aux` deleted, and
    /// delete its content from their storage and record the id of
    /// this event (one that deleted it) instead. The p2p and other
    /// protocols should accommodate such missing events as a core
    /// feature of the protocol and no longer return new data. If
    /// the `content_hash` is non-zero, the new event logically
    /// replaces the old event, but exact meaning of it depends
    /// on the `kind` of both events and is out of scope of the
    /// core event handling, and more of a UX consideration.
    ///
    /// All other bits MUST be 0 when producing headers, but might
    /// gain meaning in the future, so should still be accepted and
    /// ignored by client that don't understand them.
    pub flags: u8,

    /// The meaning and interpretation of the content from `content_hash`.
    pub kind: EventKind,

    pub content_len: MsgLen,

    /// Unused, reserved for future use
    pub padding: [u8; 8],

    pub author: ShortRostraId,

    /// Previous EventId, to form a close to a chain (actually DAG).
    ///
    /// It is supposed to be the *latest* `EventID` known to the client
    /// to allow traversing events (almost) in order.
    ///
    /// `EventID::ZERO` means "None" which means that there is
    /// no parent (first event ever), or the node that produced the event
    /// was not capable of knowing it. In such a case it is a job
    /// of the "active" node to connect it to the chain/DAG.
    pub parent_prev: ShortEventId,

    /// Auxiliary parent
    ///
    /// With some `flags` and `kind`s it is meant to point at the `EventID`
    /// used for replacement, attachment, or some other special meaning, as
    /// opposed to the `parent_event` which is always about exact ordering.
    ///
    /// In all cases used to potentially merge divergent chains
    /// into one DAG. Also, by pointing at some much older `EventID`
    /// it allows fetching older events, without traversing
    /// the DAG/chain one by one, potentially suffering latency
    /// of getting the data serially and
    ///
    /// Well behaved clients should try to make it point somewhat
    /// older event, to help it act as a skiplist.
    ///
    /// `EventID::ZERO` means "None", and is valid, e.g. in
    /// cases where the client does not maintain any history and
    /// relies on forwarding signed events to "active" node.
    ///
    /// It is also OK if `parent_aux == parent_prev`.
    pub parent_aux: ShortEventId,

    /// Blake3 hash of the content, usually returned
    pub content_hash: ContentHash,
}

pub type Keypair = (); // TODO
pub type Signature = (); // TODO

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum EventKind {
    /// Unspecified binary data
    Raw = 0x00,
    /// Social Post, backbone of the social network
    SocialPost = 0x10,
    SocialLike = 0x11,
    SocialRepost = 0x12,
    SocialComment = 0x13,
    SocialAttachment = 0x14,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ContentSocialPost {
    /// List of sub-personas this post belongs to
    pub personas: BTreeSet<String>,
    pub timestamp: u32,
    pub djot_content: String,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ContentSocialLike {
    /// List of sub-personas this post belongs to
    pub personas: BTreeSet<String>,
    pub timestamp: u32,
    pub author: RostraId,
    pub event_id: ShortEventId,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ContentSocialRepost {
    /// List of sub-personas this post belongs to
    pub personas: BTreeSet<String>,
    pub timestamp: u32,
    pub author: RostraId,
    pub event_id: ShortEventId,
}
