//! Deliberately narrower decoding for local text mutations, not wire ingestion.

use cbor4ii::core::dec::Read as _;
use serde::Deserialize;

use super::{PersonaId, PersonaTag, SocialPost};
use crate::ExternalEventId;

/// Exact known text-post fields; unknown, duplicate, or recursive values fail.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MutableSocialPost {
    /// Original legacy persona, not the effective tag fallback.
    p: Option<PersonaId>,
    /// Exact text.
    c: Option<String>,
    /// Original reply reference.
    r: Option<ExternalEventId>,
    /// Reactions are not text mutations.
    e: Option<String>,
    /// News metadata is outside this interface.
    u: Option<url::Url>,
    /// News metadata is outside this interface.
    h: Option<String>,
    /// Older text posts omit this field.
    #[serde(default)]
    n: bool,
    /// Original declared tags, not the effective tag fallback.
    #[serde(default)]
    t: std::collections::BTreeSet<PersonaTag>,
}

impl SocialPost {
    /// Decode a bounded, known text/reply payload for a local mutation.
    ///
    /// Unlike permissive wire ingestion this rejects unknown/duplicate fields,
    /// trailing bytes, non-map forms, reactions and news metadata. The one-MB
    /// input limit is local policy, not a change to signed-event compatibility.
    /// Legacy persona and declared tags are preserved independently.
    pub fn decode_mutable_text(bytes: &[u8]) -> Option<Self> {
        let post = Self::decode_mutable_fields(bytes)?;
        (!post.news
            && post.url.is_none()
            && post.title.is_none()
            && post.djot_content.is_some()
            && post.reaction.is_none())
        .then_some(post)
    }

    /// Decode only an exact, bounded, known reaction payload for local removal.
    /// This compatibility check is not Unicode emoji validation.
    pub fn decode_mutable_reaction(bytes: &[u8]) -> Option<Self> {
        let post = Self::decode_mutable_fields(bytes)?;
        (!post.news
            && post.url.is_none()
            && post.title.is_none()
            && post.djot_content.is_none()
            && post.reaction.as_deref().is_some_and(|reaction| {
                Self::is_reaction(&post.reply_to, reaction) == Some(reaction)
            }))
        .then_some(post)
    }

    /// Decode known bounded news content for native presentation and voting.
    /// Historical optional titles, URLs, and persona tags remain supported;
    /// replies, reactions, unknown fields and malformed CBOR do not.
    pub fn decode_supported_news(bytes: &[u8]) -> Option<Self> {
        let post = Self::decode_mutable_fields(bytes)?;
        (post.news && post.reply_to.is_none() && post.reaction.is_none()).then_some(post)
    }

    fn decode_mutable_fields(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 1_000_000 || bytes.first()? >> 5 != 5 {
            return None;
        }
        let mut decoder =
            cbor4ii::serde::Deserializer::new(cbor4ii::core::utils::SliceReader::new(bytes));
        let post = MutableSocialPost::deserialize(&mut decoder).ok()?;
        let mut reader = decoder.into_inner();
        let remaining = match reader.fill(1).ok()? {
            cbor4ii::core::dec::Reference::Long(bytes)
            | cbor4ii::core::dec::Reference::Short(bytes) => !bytes.is_empty(),
        };
        if remaining {
            return None;
        }
        Some(Self {
            persona: post.p,
            djot_content: post.c,
            reply_to: post.r,
            reaction: post.e,
            url: post.u,
            title: post.h,
            news: post.n,
            persona_tags: post.t,
        })
    }
}
