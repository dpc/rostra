use core::fmt;

use ::pkarr::PublicKey;

use super::{RostraId, RostraIdSecretKey};
type PkarrResult<T> = std::result::Result<T, ::pkarr::Error>;

impl RostraId {
    pub fn try_from_pkarr_str(s: &str) -> PkarrResult<Self> {
        Ok(Self(PublicKey::try_from(s)?.to_bytes()))
    }
}

impl fmt::Display for RostraId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        pkarr::PublicKey::from(*self).fmt(f)
    }
}

impl From<RostraIdSecretKey> for pkarr::Keypair {
    fn from(key: RostraIdSecretKey) -> Self {
        pkarr::Keypair::from_secret_key(&key.0)
    }
}

impl From<RostraId> for pkarr::PublicKey {
    fn from(value: RostraId) -> Self {
        pkarr::PublicKey::try_from(value.0.as_slice()).expect("RostraId must be always valid")
    }
}

impl From<pkarr::PublicKey> for RostraId {
    fn from(value: pkarr::PublicKey) -> Self {
        Self(value.to_bytes())
    }
}
