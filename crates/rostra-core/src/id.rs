use core::fmt;
use std::str::FromStr;

use snafu::Snafu;

use crate::{define_array_type_public, impl_array_type_serde, EventId, ShortEventId};

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "pkarr")]
mod pkarr;

#[cfg(feature = "serde")]
mod serde;

define_array_type_public!(struct RostraId, 32);
impl_array_type_serde!(struct RostraId,  32);
impl RostraId {
    pub const ZERO: Self = Self([0u8; 32]);
    pub const MAX: Self = Self([0xffu8; 32]);
    pub const BECH32_HRP: bech32::Hrp = bech32::Hrp::parse_unchecked("rstr");
}

impl fmt::Display for RostraId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match bech32::encode_to_fmt::<bech32::Bech32m, _>(f, Self::BECH32_HRP, &self.0) {
            Ok(()) => Ok(()),
            Err(e) => match e {
                bech32::EncodeError::TooLong(_) => unreachable!("Fixed size"),
                bech32::EncodeError::Fmt(error) => Err(error),
                e => panic!("Unexpected error: {e:#}"),
            },
        }
    }
}

#[derive(Debug, Snafu, Clone)]
pub enum RostraIdParseError {
    #[snafu(transparent)]
    Decoding {
        source: bech32::DecodeError,
    },
    InvalidHrp,
    InvalidLength,
}

impl RostraId {
    fn from_bech32m_str(s: &str) -> Result<Self, RostraIdParseError> {
        let (hrp, bytes) = bech32::decode(s)?;
        if hrp != Self::BECH32_HRP {
            return Err(InvalidHrpSnafu.build());
        }
        if bytes.len() != 32 {
            return Err(InvalidLengthSnafu.build());
        }
        Ok(Self(bytes.try_into().expect("Just checked length")))
    }

    pub fn to_z32_string(&self) -> String {
        z32::encode(&self.0)
    }
}

impl FromStr for RostraId {
    type Err = RostraIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match Self::from_bech32m_str(s) {
            Ok(o) => Ok(o),
            Err(err) => {
                // Fallback attempting decoding with older z32 (pkarr) encoding
                let bytes = z32::decode(s.as_bytes()).map_err(|_| err.clone())?;
                if bytes.len() != 32 {
                    return Err(err);
                }

                Ok(Self(bytes.try_into().expect("Just checked length")))
            }
        }
    }
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

define_array_type_public!(struct RostraIdSecretKey, 32);
impl_array_type_serde!(struct RostraIdSecretKey, 32);
impl fmt::Display for RostraIdSecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            &bip39::Mnemonic::from_entropy(self.0.as_slice())
                .expect("Fixed len, can't fail")
                .to_string(),
        )
    }
}

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

/// Full, external event id
///
/// Combination of [`crate::RostraId`] of the author and [`crate::ShortEventId`]
/// of the [`crate::Event`] that makes it possible to possibly fetch the event
/// by anyone.
///
/// Encoded as a concatenation/tuple of the two.
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalEventId((RostraId, ShortEventId));

impl ExternalEventId {
    pub fn new(rostra_id: RostraId, event_id: impl Into<ShortEventId>) -> Self {
        Self((rostra_id, event_id.into()))
    }
    pub fn rostra_id(self) -> RostraId {
        self.0 .0
    }

    pub fn event_id(self) -> ShortEventId {
        self.0 .1
    }
}
#[derive(Debug, Snafu)]
pub enum ExternalEventIdParseError {
    InvalidParts,
    RostraId,
    EventId,
}

impl FromStr for ExternalEventId {
    type Err = ExternalEventIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((author, event)) = s.split_once('-') else {
            return InvalidPartsSnafu.fail();
        };

        Ok(ExternalEventId::new(
            RostraId::from_str(author).map_err(|_| RostraIdSnafu.build())?,
            ShortEventId::from_str(event).map_err(|_| EventIdSnafu.build())?,
        ))
    }
}

impl fmt::Display for ExternalEventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Dash was picked because it well uurlencodes and such
        f.write_fmt(format_args!("{}-{}", self.0 .0, self.0 .1))
    }
}
