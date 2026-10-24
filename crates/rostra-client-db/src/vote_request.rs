//! Exact-target singleton votes with a separate local receipt namespace.

use rostra_core::event::SocialVote;
use rostra_core::id::RostraId;
use rostra_core::{EventId, ExternalEventId};

/// Caller-persisted random key, never recycled within this account database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoteRequestId(pub [u8; 16]);

/// Immutable vote input; no implicit replacement-history retargeting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteRequest {
    /// Full retained author and event ID, checked before shortening for the
    /// wire.
    pub target: (RostraId, EventId),
    /// Up, down, or neutral (clear), respectively.
    pub value: Option<bool>,
}

impl VoteRequest {
    /// Construct the existing singleton payload; this does not validate the
    /// target.
    pub fn to_social_vote(&self) -> SocialVote {
        SocialVote::new(
            ExternalEventId::new(self.target.0, self.target.1),
            self.value,
        )
    }

    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.vote_request.input.v1");
        hash.update(&[1]);
        hash.update(account.as_slice());
        hash.update(self.target.0.as_slice());
        hash.update(self.target.1.as_slice());
        hash.update(&[match self.value {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        }]);
        *hash.finalize().as_bytes()
    }
}
