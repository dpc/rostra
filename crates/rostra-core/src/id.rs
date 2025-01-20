use snafu::Snafu;

use crate::{define_array_type_public, define_array_type_public_no_serde, EventId, ShortEventId};

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "pkarr")]
mod pkarr;

#[cfg(feature = "serde")]
mod serde;

define_array_type_public_no_serde!(struct RostraId, 32);

impl RostraId {
    pub const ZERO: Self = Self([0u8; 32]);
    pub const MAX: Self = Self([0xffu8; 32]);
}

impl From<RostraId> for ShortRostraId {
    fn from(id: RostraId) -> Self {
        id.split().0
    }
}

define_array_type_public!(struct ShortRostraId, 16);
define_array_type_public!(struct RestRostraId, 16);

impl ShortRostraId {
    pub const ZERO: Self = Self([0u8; 16]);
    pub const MAX: Self = Self([0xffu8; 16]);
}

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

#[derive(Debug, Snafu)]
#[snafu(display("SecretKey error: {msg}"))]
pub struct RostraIdSecretKeyError {
    msg: String,
}

impl AsRef<str> for RostraIdSecretKeyError {
    fn as_ref(&self) -> &str {
        self.msg.as_str()
    }
}

impl From<String> for RostraIdSecretKeyError {
    fn from(msg: String) -> Self {
        Self { msg }
    }
}

pub trait ToShort {
    type ShortId;
    fn to_short(self) -> Self::ShortId;
}

impl ToShort for ShortRostraId {
    type ShortId = Self;

    fn to_short(self) -> Self::ShortId {
        self
    }
}

impl ToShort for RostraId {
    type ShortId = ShortRostraId;

    fn to_short(self) -> Self::ShortId {
        self.into()
    }
}
impl ToShort for ShortEventId {
    type ShortId = Self;

    fn to_short(self) -> Self::ShortId {
        self
    }
}

impl ToShort for EventId {
    type ShortId = ShortEventId;

    fn to_short(self) -> Self::ShortId {
        self.into()
    }
}
