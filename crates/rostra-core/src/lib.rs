use std::fmt;
use std::str::FromStr;

#[cfg(feature = "bincode")]
pub mod bincode;
pub mod event;

#[cfg(feature = "rand")]
pub mod rand;

/// Version of [`define_array_type`] that does not derive Serde
///
/// Because some types can't serde. :/
pub mod id;

pub mod macros;
pub use crate::event::Event;
pub use crate::id::ExternalEventId;

array_type_define_public!(
    struct EventId, 32
);
array_type_impl_base32_str!(EventId);

impl From<blake3::Hash> for EventId {
    fn from(value: blake3::Hash) -> Self {
        Self(value.as_bytes()[..32].try_into().expect("Must be 32 bytes"))
    }
}

impl From<EventId> for [u8; 32] {
    fn from(value: EventId) -> Self {
        value.0
    }
}

impl From<EventId> for ShortEventId {
    fn from(value: EventId) -> Self {
        Self(value.0[..16].try_into().expect("Must be 16 bytes"))
    }
}

array_type_define_public!(
    /// [`ShortEventId`] is short (16B) because it is always used in a context of an existing
    /// [`id::RostraId`] so even though client might potentially grind collisions
    /// (64-bits of resistance) it really gains them nothing.
    ///
    /// One might think of a `FullEventId` = `(RostraId, EventId)`, where
    /// the `RostraId` is passed separately or known in the context.
    ///
    /// However non-naive applications should probably store event in a smarter
    /// way anyway, e.g. by first 8B mapping to a sequence of matching events.
    struct ShortEventId, 16
);
array_type_impl_serde!(struct ShortEventId, 16);
array_type_impl_base32_str!(ShortEventId);
array_type_impl_zero_default!(ShortEventId, 16);

array_type_define_public!(
    /// Blake3 hash of a payload
    ///
    /// For actual content, see [`EventContent`]
    struct ContentHash, 32
);
array_type_impl_serde!(struct ContentHash, 32);
array_type_impl_base32_str!(ContentHash);

impl From<blake3::Hash> for ContentHash {
    fn from(value: blake3::Hash) -> Self {
        Self(value.as_bytes()[..32].try_into().expect("Must be 32 bytes"))
    }
}

impl From<ContentHash> for [u8; 32] {
    fn from(value: ContentHash) -> Self {
        value.0
    }
}

impl From<ContentHash> for blake3::Hash {
    fn from(value: ContentHash) -> Self {
        blake3::Hash::from_bytes(value.0)
    }
}

/// Length of a message, encoded in a fixed-size way
///
/// In a couple of places it of the protocol it's important
/// that a 32-bit length field is encoded as fixed-size.
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MsgLen(pub u32);

impl From<u32> for MsgLen {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<MsgLen> for u32 {
    fn from(value: MsgLen) -> Self {
        value.0
    }
}

/// Timestatmp encoded in fixed-sized way
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimestampFixed(pub u64);

impl From<u64> for TimestampFixed {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<TimestampFixed> for u64 {
    fn from(value: TimestampFixed) -> Self {
        value.0
    }
}

impl From<Timestamp> for TimestampFixed {
    fn from(value: Timestamp) -> Self {
        Self(value.0)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);
    
    pub fn now() -> Self {
        Self(time::OffsetDateTime::now_utc().unix_timestamp() as u64)
    }
}

impl From<u64> for Timestamp {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Timestamp> for u64 {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl From<TimestampFixed> for Timestamp {
    fn from(value: TimestampFixed) -> Self {
        Self(value.0)
    }
}

impl FromStr for Timestamp {
    type Err = <u64 as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(u64::from_str(s)?))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct NullableShortEventId(Option<ShortEventId>);

impl From<ShortEventId> for NullableShortEventId {
    fn from(value: ShortEventId) -> Self {
        if value == ShortEventId::ZERO {
            Self(None)
        } else {
            Self(Some(value))
        }
    }
}

impl From<NullableShortEventId> for Option<ShortEventId> {
    fn from(value: NullableShortEventId) -> Self {
        value.0
    }
}

impl fmt::Display for NullableShortEventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(id) = self.0 {
            id.fmt(f)?;
        } else {
            f.write_str("none")?;
        }
        Ok(())
    }
}
