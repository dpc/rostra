//! Immutable account-local shoutbox publication intents.

use rostra_core::event::EventAuxKey;
use rostra_core::event::content_kind::Shoutbox;
use rostra_core::id::RostraId;

/// Caller-persisted random key in the separate shoutbox namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShoutboxRequestId(pub [u8; 16]);

impl ShoutboxRequestId {
    /// Nonsingleton nonce separating equal head/time/body user intents.
    pub fn event_aux_key(self, account: RostraId) -> EventAuxKey {
        let mut hash = blake3::Hasher::new_derive_key("rostra.shoutbox_request.event_nonce.v1");
        hash.update(account.as_slice());
        hash.update(&self.0);
        let mut nonce = [0; 16];
        nonce.copy_from_slice(&hash.finalize().as_bytes()[..16]);
        EventAuxKey::from_bytes(nonce)
    }
}

/// Normalize before persisting this exact input with its immutable request key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShoutboxRequest {
    /// Already trimmed text, containing 1..=1000 UTF-8 bytes.
    pub body: String,
}

impl ShoutboxRequest {
    /// Validate exact persisted input without silently normalizing a retry.
    pub fn to_shoutbox(&self) -> crate::DbResult<Shoutbox> {
        if !(1..=1000).contains(&self.body.len()) || self.body.trim() != self.body {
            return Err(crate::DbError::ShoutboxInvalidInput);
        }
        Ok(Shoutbox {
            djot_content: self.body.clone(),
        })
    }

    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.shoutbox_request.input.v1");
        hash.update(&[1]);
        hash.update(account.as_slice());
        hash.update(&(self.body.len() as u64).to_be_bytes());
        hash.update(self.body.as_bytes());
        *hash.finalize().as_bytes()
    }
}
