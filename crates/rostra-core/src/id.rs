use core::fmt;
use std::str::FromStr;

use data_encoding::Specification;
use snafu::{OptionExt as _, Snafu};

use crate::{array_type_define_public, array_type_impl_serde, EventId, ShortEventId};

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "pkarr")]
mod pkarr;

#[cfg(feature = "serde")]
mod serde;

pub fn z32_encoding() -> data_encoding::Encoding {
    let mut spec = Specification::new();
    spec.symbols.push_str("ybndrfg8ejkmcpqxot1uwisza345h769");
    spec.encoding().unwrap()
}

array_type_define_public!(struct RostraId, 32);
array_type_impl_serde!(struct RostraId, 32);
impl RostraId {
    // Obsolete
    pub const BECH32_HRP: bech32::Hrp = bech32::Hrp::parse_unchecked("rstr");
}

impl fmt::Display for RostraId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rs")?;
        let str = z32::encode(self.0.as_slice());
        f.write_str(&str)?;
        let str_data_encoding = z32_encoding().encode(self.as_slice());
        assert_eq!(str, str_data_encoding);
        Ok(())
    }
}

#[derive(Debug, Snafu, Clone)]
pub enum RostraIdParseError {
    #[snafu(transparent)]
    DecodingBech32 {
        source: bech32::DecodeError,
    },
    DecodingZ32,
    InvalidHrp,
    InvalidLength,
    InvalidPrefix,
}

impl RostraId {
    fn from_rs_z32_str(s: &str) -> Result<Self, RostraIdParseError> {
        if !s.starts_with("rs") {
            return Err(InvalidPrefixSnafu.build());
        }

        let bytes = z32::decode(&s.as_bytes()[2..])
            .ok()
            .context(DecodingZ32Snafu)?;

        if bytes.len() != 32 {
            return Err(InvalidLengthSnafu.build());
        }
        Ok(Self(bytes.try_into().expect("Just checked length")))
    }
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

    pub fn from_unprefixed_z32_str(s: &str) -> Result<Self, RostraIdParseError> {
        // Fallback attempting decoding with unprefixed-older z32 (pkarr) encoding
        let bytes = z32::decode(s.as_bytes()).ok().context(DecodingZ32Snafu)?;
        if bytes.len() != 32 {
            return Err(InvalidLengthSnafu.build());
        }
        Ok(Self(bytes.try_into().expect("Just checked length")))
    }

    pub fn to_unprefixed_z32_string(&self) -> String {
        z32::encode(&self.0)
    }

    pub fn to_bech32_string(&self) -> String {
        match bech32::encode::<bech32::Bech32m>(RostraId::BECH32_HRP, &self.0) {
            Ok(s) => s,
            Err(e) => match e {
                bech32::EncodeError::TooLong(_) => unreachable!("Fixed size"),
                e => panic!("Unexpected error: {e:#}"),
            },
        }
    }
}

impl FromStr for RostraId {
    type Err = RostraIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match Self::from_rs_z32_str(s) {
            Ok(o) => Ok(o),
            Err(err) => {
                if let Ok(o) = Self::from_bech32m_str(s) {
                    return Ok(o);
                }
                if let Ok(o) = Self::from_unprefixed_z32_str(s) {
                    return Ok(o);
                }

                Err(err)
            }
        }
    }
}

impl From<RostraId> for ShortRostraId {
    fn from(id: RostraId) -> Self {
        id.split().0
    }
}

array_type_define_public!(struct ShortRostraId, 16);

array_type_define_public!(struct RestRostraId, 16);

impl fmt::Display for ShortRostraId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rs")?;
        let str = z32::encode(self.0.as_slice());
        f.write_str(&str)?;

        let str_data_encoding = z32_encoding().encode(self.as_slice());

        assert_eq!(str, str_data_encoding);

        Ok(())
    }
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

array_type_define_public!(struct RostraIdSecretKey, 32);
array_type_impl_serde!(struct RostraIdSecretKey, 32);
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
