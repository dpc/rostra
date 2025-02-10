use std::collections::BTreeSet;
use std::str::FromStr as _;

use snafu::Snafu;

use super::{EventContent, EventKind, PersonaId};
use crate::id::RostraId;
use crate::{
    array_type_define, array_type_impl_base32_str, array_type_impl_serde, ExternalEventId,
    ShortEventId,
};

#[derive(Debug, Snafu)]
pub struct ContentValidationError;

pub type ContentValidationResult<T> = std::result::Result<T, ContentValidationError>;

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
    fn serialize_cbor(&self) -> ContentValidationResult<EventContent> {
        self.validate()?;
        let mut buf = Vec::with_capacity(128);
        cbor4ii::serde::to_writer(&mut buf, self).expect("Can't fail");
        // ciborium::into_writer(self, &mut buf).expect("Can't fail");
        Ok(EventContent::new(buf))
    }

    fn validate(&self) -> ContentValidationResult<()> {
        Ok(())
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

impl EventContentKind for Unfollow {
    const KIND: EventKind = EventKind::UNFOLLOW;
}

array_type_define!(
    /// To avoid importing whole iroh to `rostra-core` we define our own type
    /// for `iroh::NodeAddr`
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct IrohNodeId, 32
);
array_type_impl_serde!(struct IrohNodeId, 32);
array_type_impl_base32_str!(IrohNodeId);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "t"))]
pub enum NodeAnnouncement {
    #[serde(rename = "i")]
    Iroh {
        #[serde(rename = "a")]
        addr: IrohNodeId,
    },
}

impl EventContentKind for NodeAnnouncement {
    const KIND: EventKind = EventKind::NODE_ANNOUNCEMENT;
}

// Workaround https://github.com/serde-rs/serde/issues/2704
#[cfg(feature = "serde")]
impl<'de> ::serde::de::Deserialize<'de> for NodeAnnouncement {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        #[derive(::serde::Deserialize)]
        struct NodeAnnouncementRaw<T> {
            #[serde(rename = "t")]
            t: String,
            #[serde(rename = "a")]
            addr: Option<T>,
        }

        let addr = if d.is_human_readable() {
            let raw = NodeAnnouncementRaw::<String>::deserialize(d)?;
            if raw.t != "i" {
                return Err(::serde::de::Error::custom(format!(
                    "Unknown variant: {}",
                    raw.t
                )));
            }

            let Some(addr) = raw.addr else {
                return Err(::serde::de::Error::custom("Missing field: a"));
            };
            IrohNodeId::from_str(&addr)
                .map_err(|e| ::serde::de::Error::custom(format!("Decoding a error: {}", e)))?
        } else {
            let raw = NodeAnnouncementRaw::<serde_bytes::ByteArray<32>>::deserialize(d)?;
            if raw.t != "i" {
                return Err(::serde::de::Error::custom(format!(
                    "Unknown variant: {}",
                    raw.t
                )));
            }

            let Some(addr) = raw.addr else {
                return Err(::serde::de::Error::custom("Missing field: a"));
            };

            IrohNodeId::from_bytes(addr.into_array())
        };

        Ok(NodeAnnouncement::Iroh { addr })
    }
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialPost {
    #[serde(rename = "p")]
    pub persona: PersonaId,
    #[serde(rename = "c")]
    pub djot_content: String,
    #[serde(rename = "r")]
    pub reply_to: Option<ExternalEventId>,
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
    #[serde(rename = "a")]
    pub avatar: Option<(String, Vec<u8>)>,
}

impl EventContentKind for SocialProfileUpdate {
    const KIND: EventKind = EventKind::SOCIAL_PROFILE_UPDATE;

    fn validate(&self) -> ContentValidationResult<()> {
        if 100 < self.display_name.len() {
            return Err(ContentValidationError);
        }

        if 1000 < self.display_name.len() {
            return Err(ContentValidationError);
        }

        if let Some(avatar) = self.avatar.as_ref() {
            if 100 < avatar.0.len() {
                return Err(ContentValidationError);
            }

            if !avatar.0.starts_with("image/") {
                return Err(ContentValidationError);
            }
            if 1_000_000 < avatar.1.len() {
                return Err(ContentValidationError);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
