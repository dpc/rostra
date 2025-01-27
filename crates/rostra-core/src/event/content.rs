use std::collections::BTreeSet;

use super::{EventKind, PersonaId};
use crate::id::RostraId;
use crate::ShortEventId;

#[cfg(feature = "bincode")]
pub trait Content: ::bincode::Encode + ::bincode::Decode {
    const KIND: EventKind;
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Follow {
    pub followee: RostraId,
    pub persona: PersonaId,
}

impl Content for Follow {
    const KIND: EventKind = EventKind::FOLLOW;
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Unfollow {
    pub followee: RostraId,
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialPost {
    pub persona: PersonaId,
    pub djot_content: String,
}

impl Content for SocialPost {
    const KIND: EventKind = EventKind::SOCIAL_POST;
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialUpvote {
    /// List of sub-personas this post belongs to
    pub personas: BTreeSet<String>,
    pub timestamp: u32,
    pub author: RostraId,
    pub event_id: ShortEventId,
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialRepost {
    /// List of sub-personas this post belongs to
    pub personas: BTreeSet<String>,
    pub timestamp: u32,
    pub author: RostraId,
    pub event_id: ShortEventId,
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialProfileUpdate {
    pub display_name: String,
    pub bio: String,
    pub img_mime: String,
    pub img: Vec<u8>,
}

impl Content for SocialProfileUpdate {
    const KIND: EventKind = EventKind::SOCIAL_PROFILE_UPDATE;
}
