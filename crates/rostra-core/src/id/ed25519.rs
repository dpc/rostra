use ed25519_dalek::{SigningKey, VerifyingKey};

use super::{RostraId, RostraIdSecretKey};

impl RostraIdSecretKey {
    #[cfg(feature = "rand")]
    pub fn generate() -> Self {
        SigningKey::generate(&mut rand::rng()).into()
    }

    pub fn id(self) -> RostraId {
        SigningKey::from(self).verifying_key().into()
    }
}

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

impl From<SigningKey> for RostraIdSecretKey {
    fn from(value: SigningKey) -> Self {
        Self(value.to_bytes())
    }
}
