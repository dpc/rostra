use ::pkarr::PublicKey;

use super::{RostraId, RostraIdSecretKey};
type PkarrResult<T> = std::result::Result<T, ::pkarr::Error>;

impl RostraId {
    pub fn try_from_pkarr_str(s: &str) -> PkarrResult<Self> {
        Ok(Self(PublicKey::try_from(s)?.to_bytes()))
    }
}

impl From<RostraIdSecretKey> for pkarr::Keypair {
    fn from(key: RostraIdSecretKey) -> Self {
        pkarr::Keypair::from_secret_key(&key.0)
    }
}

impl TryFrom<RostraId> for pkarr::PublicKey {
    type Error = pkarr::Error;

    fn try_from(id: RostraId) -> Result<Self, Self::Error> {
        pkarr::PublicKey::try_from(&id.0)
    }
}
