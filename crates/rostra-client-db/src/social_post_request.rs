//! Versioned local input identity for crash-safe text-post publication.

use std::collections::BTreeSet;

use rostra_core::event::PersonaTag;
use rostra_core::event::content_kind::SocialPost;
use rostra_core::id::RostraId;
use rostra_core::{EventId, ExternalEventId};

/// A caller-generated random 128-bit ID, persisted with input before
/// publication.
///
/// Never recycle IDs within the same account database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialPostRequestId(pub [u8; 16]);

/// Exact V1 text input; reactions and edits are separate operations.
///
/// Persist this immutable input until acknowledgment. A newer draft is not this
/// pending request. Tags are a set; body and tag bytes are not normalized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocialPostRequest {
    /// Exact UTF-8 text, including whitespace.
    pub body: String,
    /// Full retained target identity, checked before shortening for the wire.
    pub reply_to: Option<(RostraId, EventId)>,
    /// Persona tags in canonical lexical set order.
    pub persona_tags: BTreeSet<PersonaTag>,
}

impl SocialPostRequest {
    /// Construct the text-only wire payload; this does not validate the target.
    pub fn to_social_post(&self) -> SocialPost {
        SocialPost::new_text(
            self.body.clone(),
            self.reply_to
                .map(|(author, event)| ExternalEventId::new(author, event)),
            self.persona_tags.clone(),
        )
    }

    /// Stable V1 computational input identity, assuming BLAKE3 collision and
    /// second-preimage resistance. Never change this encoding for V1 receipts.
    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.social_post_idempotent.input.v1");
        hash.update(&[1, 0]); // V1, text mode.
        hash.update(account.as_slice());
        hash.update(&(self.body.len() as u64).to_be_bytes());
        hash.update(self.body.as_bytes());
        match self.reply_to {
            None => {
                hash.update(&[0]);
            }
            Some((author, event)) => {
                hash.update(&[1]);
                hash.update(author.as_slice());
                hash.update(event.as_slice());
            }
        }
        hash.update(&(self.persona_tags.len() as u64).to_be_bytes());
        for tag in &self.persona_tags {
            let tag = tag.as_str();
            hash.update(&(tag.len() as u64).to_be_bytes());
            hash.update(tag.as_bytes());
        }
        *hash.finalize().as_bytes()
    }
}
