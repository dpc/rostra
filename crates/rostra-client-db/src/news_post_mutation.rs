//! Exact own-news body corrections in a namespace independent of text
//! mutations.

use rostra_core::EventId;
use rostra_core::event::SocialPost;
use rostra_core::id::RostraId;

/// Random caller-persisted key in the account's news-mutation-only namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewsPostMutationRequestId(pub [u8; 16]);

/// Persist this input, its request ID and its news operation discriminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewsPostMutationRequest {
    /// Exact full retained canonical version, never an event prefix.
    pub expected_current: EventId,
    /// Explicit body correction or deletion, never inferred from body content.
    pub mutation: NewsPostMutation,
}

/// Body-only correction and deletion using existing SocialPost wire semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewsPostMutation {
    /// Exact nonblank UTF-8; preserve all supported original metadata.
    Edit {
        /// Body bytes, at most one MB before encoding.
        body: String,
    },
    /// Emit a metadata-free blank text tombstone, not a blank news replacement.
    Delete,
}

impl NewsPostMutationRequest {
    /// Stable V1 digest in a domain distinct from legacy text mutation
    /// receipts.
    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.news_post_mutation.input.v1");
        hash.update(&[1]);
        hash.update(account.as_slice());
        hash.update(self.expected_current.as_slice());
        match &self.mutation {
            NewsPostMutation::Edit { body } => {
                hash.update(&[0]);
                hash.update(&(body.len() as u64).to_be_bytes());
                hash.update(body.as_bytes());
            }
            NewsPostMutation::Delete => {
                hash.update(&[1]);
            }
        }
        *hash.finalize().as_bytes()
    }

    /// Reject blank corrections instead of accidentally emitting a deletion.
    pub fn validate(&self) -> crate::DbResult<()> {
        if let NewsPostMutation::Edit { body } = &self.mutation {
            if body.len() > 1_000_000 {
                return Err(crate::DbError::SocialPostMutationTooLarge);
            }
            if body.trim().is_empty() {
                return Err(crate::DbError::SocialPostMutationEmpty);
            }
        }
        Ok(())
    }

    /// Build the exact candidate from already-validated original metadata.
    ///
    /// This does not authorize or reserve a target. The database independently
    /// repeats preparation and compares the complete candidate before commit.
    pub fn apply(&self, mut post: SocialPost) -> SocialPost {
        match &self.mutation {
            NewsPostMutation::Edit { body } => {
                post.djot_content = Some(body.clone());
                post
            }
            NewsPostMutation::Delete => {
                SocialPost::new_text(String::new(), None, Default::default())
            }
        }
    }
}
