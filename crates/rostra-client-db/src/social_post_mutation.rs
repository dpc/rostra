//! Immutable, account-local own-text mutation inputs.

use rostra_core::EventId;
use rostra_core::id::RostraId;

/// Random caller-generated key in the mutation-only account namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialPostMutationRequestId(pub [u8; 16]);

/// Persist this exact input and its request ID before invoking publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocialPostMutationRequest {
    /// Full retained identity of the expected current version, never a prefix.
    pub expected_current: EventId,
    /// Explicit operation; blank text is not implicit deletion.
    pub mutation: SocialPostMutation,
}

/// Text edits and deletions; reactions and news are intentionally separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocialPostMutation {
    /// Preserve exact nonblank text and all supported original metadata.
    Edit {
        /// Exact UTF-8 bytes, limited to one MB before encoding.
        body: String,
    },
    /// Emit a blank text tombstone that deletes the expected auxiliary parent.
    Delete,
}

impl SocialPostMutationRequest {
    /// Stable domain-separated V1 digest; never retain a plaintext input
    /// journal.
    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.social_post_mutation.input.v1");
        hash.update(&[1]);
        hash.update(account.as_slice());
        hash.update(self.expected_current.as_slice());
        match &self.mutation {
            SocialPostMutation::Edit { body } => {
                hash.update(&[0]);
                hash.update(&(body.len() as u64).to_be_bytes());
                hash.update(body.as_bytes());
            }
            SocialPostMutation::Delete => {
                hash.update(&[1]);
            }
        }
        *hash.finalize().as_bytes()
    }

    /// Validate the bounded edit input without consulting target state.
    pub fn validate(&self) -> crate::DbResult<()> {
        if let SocialPostMutation::Edit { body } = &self.mutation {
            if body.len() > 1_000_000 {
                return Err(crate::DbError::SocialPostMutationTooLarge);
            }
            if body.trim().is_empty() {
                return Err(crate::DbError::SocialPostMutationEmpty);
            }
        }
        Ok(())
    }
}
