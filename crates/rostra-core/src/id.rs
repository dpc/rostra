use crate::{define_array_type_public, define_array_type_public_no_serde};

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "pkarr")]
mod pkarr;

#[cfg(feature = "serde")]
mod serde;

define_array_type_public_no_serde!(struct RostraId, 32);

impl From<RostraId> for ShortRostraId {
    fn from(id: RostraId) -> Self {
        id.split().0
    }
}

define_array_type_public!(struct ShortRostraId, 16);
define_array_type_public!(struct RestRostraId, 16);

impl RostraId {
    pub fn split(self) -> (ShortRostraId, RestRostraId) {
        (
            ShortRostraId(self.0[0..16].try_into().expect("Can't fail")),
            RestRostraId(self.0[16..32].try_into().expect("Can't fail")),
        )
    }

    pub fn assemble(short: ShortRostraId, rest: RestRostraId) -> Self {
        Self([short.0, rest.0].concat().try_into().expect("Can't fail"))
    }
}

define_array_type_public_no_serde!(struct RostraIdSecretKey, 32);

#[derive(Debug)]
pub struct RostraIdSecretKeyError(String);
