//! Immutable local reaction requests; independent adds are never toggles.

use rostra_core::event::SocialPost;
use rostra_core::id::RostraId;
use rostra_core::{EventId, ExternalEventId};

/// Caller-persisted random key in this account's reaction-only namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReactionRequestId(pub [u8; 16]);

impl ReactionRequestId {
    /// Domain-separated nonce in the existing nonsingleton header's auxiliary
    /// key. Distinct accepted Add intents must not collapse at equal
    /// heads/time. This does not set the singleton flag or expose the raw
    /// local request ID.
    pub fn event_aux_key(self, account: RostraId) -> rostra_core::event::EventAuxKey {
        let mut hash = blake3::Hasher::new_derive_key("rostra.reaction_request.event_nonce.v1");
        hash.update(account.as_slice());
        hash.update(&self.0);
        let mut nonce = [0; 16];
        nonce.copy_from_slice(&hash.finalize().as_bytes()[..16]);
        rostra_core::event::EventAuxKey::from_bytes(nonce)
    }
}

/// Persist this exact input together with its key before publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionRequest {
    /// Full retained target author and event identity, never a short prefix.
    pub target: (RostraId, EventId),
    /// Independent addition or precise own-row removal.
    pub operation: ReactionOperation,
}

/// Reaction changes are deliberately unsupported by the current wire semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReactionOperation {
    /// Add another independent reaction, including when self already reacted.
    Add {
        /// Exact legacy-compatible reaction bytes, not normalized.
        reaction: String,
    },
    /// Delete only this exact owned current reaction row.
    Remove {
        /// Full selected reaction event ID.
        expected_current: EventId,
    },
    /// Always fails without consuming a key; requires a future protocol
    /// decision.
    Change {
        /// Full selected reaction event ID.
        expected_current: EventId,
        /// Desired exact reaction bytes.
        reaction: String,
    },
}

impl ReactionRequest {
    /// Check local input policy without consulting mutable target state.
    pub fn validate(&self) -> crate::DbResult<()> {
        match &self.operation {
            ReactionOperation::Change { .. } => Err(crate::DbError::ReactionUnsupported),
            ReactionOperation::Add { reaction } => {
                if reaction.len() > 8 {
                    return Err(crate::DbError::ReactionInvalidInput);
                }
                SocialPost::new_reaction(
                    reaction.clone(),
                    ExternalEventId::new(self.target.0, self.target.1),
                )
                .ok_or(crate::DbError::ReactionInvalidInput)?;
                Ok(())
            }
            ReactionOperation::Remove { .. } => Ok(()),
        }
    }

    /// Exact auxiliary deletion parent, never a followed replacement head.
    pub fn expected_current(&self) -> Option<EventId> {
        match self.operation {
            ReactionOperation::Add { .. } => None,
            ReactionOperation::Remove { expected_current }
            | ReactionOperation::Change {
                expected_current, ..
            } => Some(expected_current),
        }
    }

    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.reaction_request.input.v1");
        hash.update(&[1]);
        hash.update(account.as_slice());
        hash.update(self.target.0.as_slice());
        hash.update(self.target.1.as_slice());
        match &self.operation {
            ReactionOperation::Add { reaction } => {
                hash.update(&[0]);
                hash.update(&(reaction.len() as u64).to_be_bytes());
                hash.update(reaction.as_bytes());
            }
            ReactionOperation::Remove { expected_current } => {
                hash.update(&[1]);
                hash.update(expected_current.as_slice());
            }
            ReactionOperation::Change {
                expected_current,
                reaction,
            } => {
                hash.update(&[2]);
                hash.update(expected_current.as_slice());
                hash.update(&(reaction.len() as u64).to_be_bytes());
                hash.update(reaction.as_bytes());
            }
        }
        *hash.finalize().as_bytes()
    }
}
