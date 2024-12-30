use core::fmt;
use std::str::FromStr;

use pkarr::{Keypair, PublicKey};

use super::{RostraId, RostraIdSecretKey};

type PkarrResult<T> = std::result::Result<T, ::pkarr::Error>;

impl RostraId {
    pub fn try_from_pkarr_str(s: &str) -> PkarrResult<Self> {
        Ok(Self(PublicKey::try_from(s)?.to_bytes()))
    }

    pub fn try_fmt(self) -> RostraIdTryFmt {
        RostraIdTryFmt(self)
    }
}

impl FromStr for RostraId {
    type Err = pkarr::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from_pkarr_str(s)
    }
}

pub struct RostraIdTryFmt(RostraId);

impl fmt::Display for RostraIdTryFmt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match PublicKey::try_from(self.0) {
            Ok(p) => p.fmt(f),
            Err(_) => f.write_str("invalid-key"),
        }
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
    type Error = pkarr::Error;

    fn try_from(id: RostraId) -> Result<Self, Self::Error> {
        PublicKey::try_from(id.as_slice())
    }
}

impl From<RostraIdSecretKey> for pkarr::Keypair {
    fn from(id_secret: RostraIdSecretKey) -> Self {
        pkarr::Keypair::from_secret_key(&id_secret.0)
    }
}
