//! Immutable local news creation, separate from ordinary text requests.

use std::collections::BTreeSet;

use rostra_core::event::{EventAuxKey, PersonaTag, SocialPost};
use rostra_core::id::RostraId;

/// Caller-persisted random key in this account's news-create namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewsPostRequestId(pub [u8; 16]);

impl NewsPostRequestId {
    /// Independent nonsingleton nonce; never used for vote singletons.
    pub fn event_aux_key(self, account: RostraId) -> EventAuxKey {
        let mut hash = blake3::Hasher::new_derive_key("rostra.news_post_request.event_nonce.v1");
        hash.update(account.as_slice());
        hash.update(&self.0);
        let mut nonce = [0; 16];
        nonce.copy_from_slice(&hash.finalize().as_bytes()[..16]);
        EventAuxKey::from_bytes(nonce)
    }
}

/// Normalize the title/URL before persisting this input and its request key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewsPostRequest {
    /// Exact body, including whitespace; empty bodies are supported.
    pub body: String,
    /// Nonempty, already trimmed title.
    pub title: String,
    /// Already trimmed, parseable URL; spelling is part of request identity.
    pub url: Option<String>,
}

impl NewsPostRequest {
    /// Validate and construct fixed-news, no-reply content without normalizing
    /// persisted input. Hosts must independently restrict navigable URL
    /// schemes.
    pub fn to_social_post(&self) -> crate::DbResult<SocialPost> {
        if self.title.is_empty() || self.title.trim() != self.title {
            return Err(crate::DbError::NewsInvalidInput);
        }
        let url = self
            .url
            .as_ref()
            .map(|url| {
                if url.is_empty() || url.trim() != url {
                    return Err(crate::DbError::NewsInvalidInput);
                }
                url.parse().map_err(|_| crate::DbError::NewsInvalidInput)
            })
            .transpose()?;
        Ok(SocialPost::new_text(
            self.body.clone(),
            None,
            BTreeSet::from([PersonaTag::new("news").expect("Valid tag")]),
        )
        .with_news_fields(url, Some(self.title.clone())))
    }

    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.news_post_request.input.v1");
        hash.update(&[1]);
        hash.update(account.as_slice());
        for text in [&self.body, &self.title] {
            hash.update(&(text.len() as u64).to_be_bytes());
            hash.update(text.as_bytes());
        }
        match &self.url {
            Some(url) => {
                hash.update(&[1]);
                hash.update(&(url.len() as u64).to_be_bytes());
                hash.update(url.as_bytes());
            }
            None => {
                hash.update(&[0]);
            }
        }
        *hash.finalize().as_bytes()
    }
}
