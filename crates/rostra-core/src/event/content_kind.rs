use std::collections::BTreeSet;
#[cfg(feature = "serde")]
use std::str::FromStr as _;

use snafu::Snafu;
use unicode_segmentation::UnicodeSegmentation as _;

#[cfg(feature = "serde")]
use super::{EventAuxKey, EventContentRaw, EventKind};
use super::{PersonaId, PersonaTag};
use crate::id::RostraId;
#[cfg(feature = "serde")]
use crate::id::ToShort as _;
use crate::{
    ExternalEventId, array_type_define, array_type_impl_base32_str, array_type_impl_serde,
};

#[derive(Debug, Snafu)]
#[snafu(display("Content validation error: {public_message}"))]
pub struct ContentValidationError {
    pub public_message: String,
}

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
    #[cfg_attr(feature = "serde", serde(rename = "i"))]
    pub followee: RostraId,
    /// Legacy persona field — kept for deserialization of old events
    #[cfg_attr(feature = "serde", serde(rename = "p"))]
    pub persona: Option<PersonaId>,
    /// Legacy persona selector — kept for deserialization of old events
    #[cfg_attr(feature = "serde", serde(rename = "s"))]
    pub selector: Option<PersonaSelector>,
    /// New tag-based selector
    #[cfg_attr(feature = "serde", serde(rename = "t", default))]
    pub persona_tags_selector: Option<PersonasTagsSelector>,
}

impl Follow {
    /// Get the legacy persona selector, for backward compat.
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
        self.persona.is_none() && self.selector.is_none() && self.persona_tags_selector.is_none()
    }
}
#[cfg(feature = "serde")]
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

impl IrohNodeId {
    /// Returns the node ID in z32 encoding (standard Iroh/Pkarr format).
    pub fn to_z32(&self) -> String {
        z32::encode(self.as_slice())
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "t"))]
pub enum NodeAnnouncement {
    #[cfg_attr(feature = "serde", serde(rename = "i"))]
    Iroh {
        #[cfg_attr(feature = "serde", serde(rename = "a"))]
        addr: IrohNodeId,
    },
}

#[cfg(feature = "serde")]
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
    /// Legacy persona field — hardcoded to 0 for new posts, kept for backward
    /// compat with old clients. Will be dropped in the future.
    #[cfg_attr(feature = "serde", serde(rename = "p", default))]
    persona: Option<PersonaId>,
    #[cfg_attr(feature = "serde", serde(rename = "c"))]
    pub djot_content: Option<String>,
    #[cfg_attr(feature = "serde", serde(rename = "r"))]
    pub reply_to: Option<ExternalEventId>,
    // "e" for "emoji"
    #[cfg_attr(feature = "serde", serde(rename = "e"))]
    pub reaction: Option<String>,
    /// Persona tags for this post
    #[cfg_attr(feature = "serde", serde(rename = "t", default))]
    persona_tags: BTreeSet<PersonaTag>,
}

impl SocialPost {
    /// Create a new `SocialPost`.
    ///
    /// If the body is a single-emoji reply, it's stored as a reaction (with
    /// `djot_content` set to `None`). Sets legacy `persona` to
    /// `Some(PersonaId(0))` for backward compat.
    pub fn new(
        body: String,
        reply_to: Option<ExternalEventId>,
        persona_tags: BTreeSet<PersonaTag>,
    ) -> Self {
        let (djot_content, reaction) = if let Some(reaction) = Self::is_reaction(&reply_to, &body) {
            (None, Some(reaction.to_owned()))
        } else {
            (Some(body), None)
        };
        Self {
            persona: Some(PersonaId(0)),
            djot_content,
            reply_to,
            reaction,
            persona_tags,
        }
    }

    /// Get the effective persona tags for this post.
    ///
    /// If `persona_tags` is non-empty, returns those. Otherwise falls back to
    /// converting the legacy `persona` field to a tag (0=personal,
    /// 1=professional, 2=civic).
    pub fn persona_tags(&self) -> BTreeSet<PersonaTag> {
        if !self.persona_tags.is_empty() {
            return self.persona_tags.clone();
        }
        if let Some(tag) = self.persona.and_then(PersonaTag::from_persona_id) {
            BTreeSet::from([tag])
        } else {
            BTreeSet::new()
        }
    }

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

#[cfg(feature = "serde")]
impl EventContentKind for SocialPost {
    const KIND: EventKind = EventKind::SOCIAL_POST;
}

/// Shoutbox post - simple broadcast message without persona, replies, or
/// reactions
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Shoutbox {
    #[cfg_attr(feature = "serde", serde(rename = "c"))]
    pub djot_content: String,
}

#[cfg(feature = "serde")]
impl EventContentKind for Shoutbox {
    const KIND: EventKind = EventKind::SHOUTBOX;

    fn validate(&self) -> ContentValidationResult<()> {
        // Limit to 1000 characters
        if 1000 < self.djot_content.len() {
            return Err(ContentValidationError {
                public_message: "Shoutbox message too long (max 1000 characters)".into(),
            });
        }
        if self.djot_content.is_empty() {
            return Err(ContentValidationError {
                public_message: "Shoutbox message cannot be empty".into(),
            });
        }
        Ok(())
    }
}

/// A piece of media (like an image, or a video)
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialMedia {
    /// Mime type for the `data`
    #[cfg_attr(feature = "serde", serde(rename = "m"))]
    pub mime: String,
    /// Binary content of the media file
    #[cfg_attr(feature = "serde", serde(rename = "d"))]
    pub data: Vec<u8>,
}

#[cfg(feature = "serde")]
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
        // Limit media file size to 200MB
        if 200 * 1024 * 1024 < self.data.len() {
            return Err(ContentValidationError {
                public_message: "File too large (max 200MB)".into(),
            });
        }

        // Validate MIME type length
        if 100 < self.mime.len() {
            return Err(ContentValidationError {
                public_message: "MIME type too long".into(),
            });
        }

        // Don't allow empty data
        if self.data.is_empty() {
            return Err(ContentValidationError {
                public_message: "File is empty".into(),
            });
        }

        Ok(())
    }
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocialProfileUpdate {
    #[cfg_attr(feature = "serde", serde(rename = "n"))]
    pub display_name: String,
    #[cfg_attr(feature = "serde", serde(rename = "b"))]
    pub bio: String,
    #[cfg_attr(feature = "serde", serde(rename = "a"))]
    pub avatar: Option<(String, Vec<u8>)>,
}

#[cfg(feature = "serde")]
impl EventContentKind for SocialProfileUpdate {
    const KIND: EventKind = EventKind::SOCIAL_PROFILE_UPDATE;

    fn validate(&self) -> ContentValidationResult<()> {
        if 100 < self.display_name.len() {
            return Err(ContentValidationError {
                public_message: "Display name too long (max 100 characters)".into(),
            });
        }

        if 1000 < self.bio.len() {
            return Err(ContentValidationError {
                public_message: "Bio too long (max 1000 characters)".into(),
            });
        }

        if let Some(avatar) = self.avatar.as_ref() {
            if 100 < avatar.0.len() {
                return Err(ContentValidationError {
                    public_message: "Avatar MIME type too long".into(),
                });
            }

            if !avatar.0.starts_with("image/") {
                return Err(ContentValidationError {
                    public_message: "Avatar must be an image".into(),
                });
            }
            if 1_000_000 < avatar.1.len() {
                return Err(ContentValidationError {
                    public_message: "Avatar too large (max 1MB)".into(),
                });
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

/// Tag-based selector for filtering posts by persona tags.
///
/// Replaces the old `PersonaSelector` for new follow events.
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PersonasTagsSelector {
    /// Only show posts that have at least one of these tags
    Only { ids: BTreeSet<PersonaTag> },
    /// Show all posts except those that have any of these tags
    Except { ids: BTreeSet<PersonaTag> },
}

impl PersonasTagsSelector {
    /// Check whether a set of tags on a post matches this selector.
    pub fn matches_tags(&self, tags: &BTreeSet<PersonaTag>) -> bool {
        match self {
            PersonasTagsSelector::Only { ids } => {
                // Post matches if it has at least one of the selected tags
                tags.iter().any(|t| ids.contains(t))
            }
            PersonasTagsSelector::Except { ids } => {
                // Post matches if it has none of the excluded tags
                !tags.iter().any(|t| ids.contains(t))
            }
        }
    }
}

impl Default for PersonasTagsSelector {
    fn default() -> Self {
        Self::Except {
            ids: BTreeSet::new(),
        }
    }
}

#[cfg(test)]
mod tests;
