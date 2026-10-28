//! Immutable exact-byte public uploads, independent of normalized image policy.

use rostra_core::event::EventAuxKey;
use rostra_core::id::RostraId;

/// Fixed untrusted-binary declaration, never inferred from the selected file.
pub const PUBLIC_ATTACHMENT_MIME: &str = "application/octet-stream";
/// Maximum original byte length; empty attachments are not supported.
pub const PUBLIC_ATTACHMENT_MAX_BYTES: usize = 1_000_000;

/// Random caller-generated ID, persisted with the exact bytes before calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicAttachmentRequestId(pub [u8; 16]);

impl PublicAttachmentRequestId {
    /// Domain-separated opaque nonce in a nonsingleton event's auxiliary key.
    ///
    /// Independent IDs do not collapse at equal heads/time/content. This does
    /// not set the singleton flag or publish the raw local request ID.
    pub fn event_aux_key(self, account: RostraId) -> EventAuxKey {
        let mut hash = blake3::Hasher::new_derive_key("rostra.public_attachment.event_nonce.v1");
        hash.update(account.as_slice());
        hash.update(&self.0);
        let mut nonce = [0; 16];
        nonce.copy_from_slice(&hash.finalize().as_bytes()[..16]);
        EventAuxKey::from_bytes(nonce)
    }
}

/// Exact public pending bytes, without filename, MIME inference or decoding.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicAttachmentRequest {
    /// Persist these original bytes unchanged across retries and restart.
    pub data: Vec<u8>,
}

impl PublicAttachmentRequest {
    /// Check the fixed nonempty original-byte limit without interpreting data.
    pub fn validate(&self) -> crate::DbResult<()> {
        if self.data.is_empty() || self.data.len() > PUBLIC_ATTACHMENT_MAX_BYTES {
            return Err(crate::DbError::PublicAttachmentInput);
        }
        Ok(())
    }

    /// Account-scoped V1 digest; the permanent receipt never retains the bytes.
    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.public_attachment.input.v1");
        hash.update(account.as_slice());
        hash.update(&(PUBLIC_ATTACHMENT_MIME.len() as u64).to_be_bytes());
        hash.update(PUBLIC_ATTACHMENT_MIME.as_bytes());
        hash.update(&(self.data.len() as u64).to_be_bytes());
        hash.update(&self.data);
        *hash.finalize().as_bytes()
    }
}
