use pkarr::{Keypair, PublicKey};

use super::{RostraId, RostraIdSecretKey};

type PkarrPublicKeyResult<T> = std::result::Result<T, ::pkarr::errors::PublicKeyError>;

impl RostraId {
    pub fn try_from_pkarr_str(s: &str) -> PkarrPublicKeyResult<Self> {
        Ok(Self(PublicKey::try_from(s)?.to_bytes()))
    }
}

impl From<Keypair> for RostraId {
    fn from(keypair: Keypair) -> Self {
        Self(keypair.public_key().to_bytes())
    }
}

impl From<PublicKey> for RostraId {
    fn from(value: PublicKey) -> Self {
        Self(value.to_bytes())
    }
}

impl TryFrom<RostraId> for PublicKey {
    type Error = pkarr::errors::PublicKeyError;

    fn try_from(id: RostraId) -> Result<Self, Self::Error> {
        PublicKey::try_from(id.as_slice())
    }
}

impl From<RostraIdSecretKey> for pkarr::Keypair {
    fn from(id_secret: RostraIdSecretKey) -> Self {
        pkarr::Keypair::from_secret_key(&id_secret.0)
    }
}
