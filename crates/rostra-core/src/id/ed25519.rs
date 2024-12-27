use ed25519_dalek::{SigningKey, VerifyingKey};

use super::{RostraId, RostraIdSecretKey};

impl From<RostraId> for VerifyingKey {
    fn from(value: RostraId) -> Self {
        VerifyingKey::try_from(value.0.as_slice()).expect("RostraId must be always valid")
    }
}

impl From<VerifyingKey> for RostraId {
    fn from(value: VerifyingKey) -> Self {
        Self(value.to_bytes())
    }
}

impl From<RostraIdSecretKey> for SigningKey {
    fn from(key: RostraIdSecretKey) -> Self {
        SigningKey::from(key.0)
    }
}
