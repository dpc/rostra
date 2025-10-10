use std::str::FromStr as _;

use snafu::Snafu;
use unicode_segmentation::UnicodeSegmentation as _;

use super::{EventAuxKey, EventContentRaw, EventKind, PersonaId};
use crate::id::{RostraId, ToShort as _};
use crate::{
    ExternalEventId, array_type_define, array_type_impl_base32_str, array_type_impl_serde,
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
    fn serialize_cbor(&self) -> ContentValidationResult<EventContentRaw> {
        self.validate()?;
        let mut buf = Vec::with_capacity(128);
        cbor4ii::serde::to_writer(&mut buf, self).expect("Can't fail");
        // ciborium::into_writer(self, &mut buf).expect("Can't fail");
        Ok(EventContentRaw::new(buf))
    }

    fn singleton_key_aux(&self) -> Option<EventAuxKey> {
        None
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
    pub persona: Option<PersonaId>,
    #[serde(rename = "s")]
    pub selector: Option<PersonaSelector>,
}

impl Follow {
    pub fn selector(self) -> Option<PersonaSelector> {
        if let Some(selector) = self.selector {
            return Some(selector);
        }
        if let Some(persona) = self.persona {
            return Some(PersonaSelector::Only { ids: vec![persona] });
        }
        None
    }

    pub fn is_unfollow(&self) -> bool {
        self.selector.is_none() && self.selector.is_none()
    }
}
impl EventContentKind for Follow {
    const KIND: EventKind = EventKind::FOLLOW;

    fn singleton_key_aux(&self) -> Option<EventAuxKey> {
        Some(EventAuxKey::from_bytes(self.followee.to_short().to_bytes()))
    }
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
                .map_err(|e| ::serde::de::Error::custom(format!("Decoding a error: {e}")))?
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
    pub djot_content: Option<String>,
    #[serde(rename = "r")]
    pub reply_to: Option<ExternalEventId>,
    // "e" for "emoji"
    #[serde(rename = "e")]
    pub reaction: Option<String>,
}

impl SocialPost {
    pub fn is_reaction<'t>(
        reply_to: &'_ Option<ExternalEventId>,
        text: &'t str,
    ) -> Option<&'t str> {
        // Can't be a reaction, if there's nothing it reacts to.
        if reply_to.is_none() {
            return None;
        }
        if 8 < text.len() {
            // Nah...
            return None;
        }
        let text = text.trim();

        // Get the first grapheme cluster
        let first_grapheme = text.graphemes(true).next()?;

        // Check if it contains only characters that *can't* be emojis
        let is_not_emoji = first_grapheme.chars().all(|c| {
            // Filter out common non-emoji characters
            //
            // Letters and digits (A-Z, a-z, 0-9)
            c.is_ascii_alphanumeric() ||
                // Common punctuation like .,!? etc.
                c.is_ascii_punctuation() ||
                // Spaces, tabs, etc.        
                c.is_ascii_whitespace() ||
                // Control chars like \n, \r        
                c.is_ascii_control()
        });

        // If it's not entirely non-emoji characters, assume it’s an emoji
        if !is_not_emoji {
            Some(first_grapheme)
        } else {
            None
        }
    }

    pub fn get_reaction(&self) -> Option<&str> {
        let reaction = self.reaction.as_ref()?.trim();

        Self::is_reaction(&self.reply_to, reaction)
    }
}

impl EventContentKind for SocialPost {
    const KIND: EventKind = EventKind::SOCIAL_POST;
}

/// A piece of media (like an image, or a video)
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialMedia {
    /// Mime type for the `data`
    #[serde(rename = "m")]
    pub mime: String,
    /// Binary content of the media file
    #[serde(rename = "d")]
    pub data: Vec<u8>,
}

impl EventContentKind for SocialMedia {
    const KIND: EventKind = EventKind::SOCIAL_MEDIA;

    fn singleton_key_aux(&self) -> Option<EventAuxKey> {
        // Use blake3 hash of the content data as the key
        let hash = blake3::hash(&self.data);
        let hash_bytes = hash.as_bytes();
        let mut key_bytes = [0u8; 16];
        key_bytes.copy_from_slice(&hash_bytes[..16]);
        Some(EventAuxKey::from_bytes(key_bytes))
    }

    fn validate(&self) -> ContentValidationResult<()> {
        // Limit media file size to 9MB to prevent issues, until
        // we have a better data management and reclamation system.
        if self.data.len() > 9 * 1024 * 1024 {
            return Err(ContentValidationError);
        }

        // Validate MIME type length
        if self.mime.len() > 100 {
            return Err(ContentValidationError);
        }

        // Don't allow empty data
        if self.data.is_empty() {
            return Err(ContentValidationError);
        }

        Ok(())
    }
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
    fn singleton_key_aux(&self) -> Option<EventAuxKey> {
        Some(EventAuxKey::ZERO)
    }
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PersonaSelector {
    Only { ids: Vec<PersonaId> },
    Except { ids: Vec<PersonaId> },
}

impl PersonaSelector {
    pub fn matches(&self, persona: PersonaId) -> bool {
        match self {
            PersonaSelector::Only { ids } => ids.contains(&persona),
            PersonaSelector::Except { ids } => !ids.contains(&persona),
        }
    }
}

impl Default for PersonaSelector {
    fn default() -> Self {
        Self::Except { ids: vec![] }
    }
}

#[cfg(test)]
mod tests;
