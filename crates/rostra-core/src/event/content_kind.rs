use std::collections::BTreeSet;

use super::{EventContent, EventKind, PersonaId};
use crate::id::RostraId;
use crate::ShortEventId;

/// Extension trait for deserializing content
#[cfg(feature = "serde")]
pub trait EventContentKind: ::serde::Serialize + ::serde::de::DeserializeOwned {
    /// The [`EventKind`] corresponding to this content kind
    const KIND: EventKind;

    /// Deserialize cbor-encoded content
    ///
    /// Most content will be deserialized a cbor, as it's:
    ///
    /// * self-describing, so flexible to evolve while maintaining compatibility
    /// * roughly-compatible with JSON, making it easy to transform to JSON form
    ///   for external APIs and such.
    fn serialize_cbor(&self) -> EventContent {
        let mut buf = Vec::with_capacity(128);
        ciborium::into_writer(self, &mut buf).expect("Can't fail");
        EventContent::new(buf)
    }
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Follow {
    #[serde(rename = "i")]
    pub followee: RostraId,
    #[serde(rename = "p")]
    pub persona: PersonaId,
}

impl EventContentKind for Follow {
    const KIND: EventKind = EventKind::FOLLOW;
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Unfollow {
    #[serde(rename = "i")]
    pub followee: RostraId,
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialPost {
    #[serde(rename = "p")]
    pub persona: PersonaId,
    #[serde(rename = "c")]
    pub djot_content: String,
}

impl EventContentKind for SocialPost {
    const KIND: EventKind = EventKind::SOCIAL_POST;
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialUpvote {
    /// List of sub-personas this post belongs to
    #[serde(rename = "p")]
    pub personas: BTreeSet<String>,
    #[serde(rename = "t")]
    pub timestamp: u32,
    #[serde(rename = "i")]
    pub author: RostraId,
    #[serde(rename = "e")]
    pub event_id: ShortEventId,
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialRepost {
    /// List of sub-personas this post belongs to
    #[serde(rename = "p")]
    pub personas: BTreeSet<String>,
    #[serde(rename = "t")]
    pub timestamp: u32,
    #[serde(rename = "i")]
    pub author: RostraId,
    #[serde(rename = "e")]
    pub event_id: ShortEventId,
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialProfileUpdate {
    #[serde(rename = "n")]
    pub display_name: String,
    #[serde(rename = "b")]
    pub bio: String,
    #[serde(rename = "m")]
    pub img_mime: String,
    #[serde(rename = "i")]
    pub img: Vec<u8>,
}

impl EventContentKind for SocialProfileUpdate {
    const KIND: EventKind = EventKind::SOCIAL_PROFILE_UPDATE;
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialComment {
    #[serde(rename = "i")]
    pub rostra_id: RostraId,
    #[serde(rename = "e")]
    pub event_id: ShortEventId,
    #[serde(rename = "p")]
    pub persona: PersonaId,
    #[serde(rename = "c")]
    pub djot_content: String,
}

impl EventContentKind for SocialComment {
    const KIND: EventKind = EventKind::SOCIAL_COMMENT;
}
