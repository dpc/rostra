//! Permanent local identity for immutable normalized image publication.

use rostra_core::id::RostraId;

/// Random caller-generated ID, persisted before publication; never recycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaRequestId(pub [u8; 16]);

/// Exact normalized pending input, not the original selected file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRequest {
    /// Exact canonical MIME (V1 requires image/png).
    pub mime: String,
    /// Exact normalized bytes, persisted before signing.
    pub data: Vec<u8>,
    /// Normalization policy; retry identity includes this version.
    pub policy_version: u32,
}

impl MediaRequest {
    /// Stable digest of exact input. Receipts never store original image bytes.
    pub(crate) fn digest(&self, account: RostraId) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("rostra.media_idempotent.input.v1");
        hash.update(account.as_slice());
        hash.update(&self.policy_version.to_be_bytes());
        hash.update(&(self.mime.len() as u64).to_be_bytes());
        hash.update(self.mime.as_bytes());
        hash.update(&(self.data.len() as u64).to_be_bytes());
        hash.update(&self.data);
        *hash.finalize().as_bytes()
    }
}
