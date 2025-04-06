use std::convert::Infallible;
use std::sync::Arc;

use snafu::Snafu;

use super::{
    ContentValidationError, EventContentKind, EventContentRaw, EventContentUnsized, EventKind,
    EventSignature, VerifiedEventContent,
};

impl serde::Serialize for EventSignature {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if ser.is_human_readable() {
            ser.serialize_str(&data_encoding::HEXLOWER.encode(&self.0))
        } else {
            ser.serialize_bytes(&self.0)
        }
    }
}

impl<'de> serde::de::Deserialize<'de> for EventSignature {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if de.is_human_readable() {
            let hex_str = String::deserialize(de)?;
            let bytes = data_encoding::HEXLOWER
                .decode(hex_str.as_bytes())
                .map_err(serde::de::Error::custom)?;

            if bytes.len() != 64 {
                return Err(serde::de::Error::custom("Invalid length"));
            }
            Ok(EventSignature(
                bytes.try_into().expect("Just checked length"),
            ))
        } else {
            let bytes = Vec::<u8>::deserialize(de)?;
            if bytes.len() != 64 {
                return Err(serde::de::Error::custom("Invalid length"));
            }
            Ok(EventSignature(bytes.try_into().expect("Just checked len")))
        }
    }
}

impl serde::Serialize for EventContentRaw {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if ser.is_human_readable() {
            ser.serialize_str(&data_encoding::HEXLOWER.encode(self.as_ref()))
        } else {
            ser.serialize_bytes(self.as_ref())
        }
    }
}

impl<'de> serde::de::Deserialize<'de> for EventContentRaw {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if de.is_human_readable() {
            let hex_str = String::deserialize(de)?;
            let bytes: Arc<[u8]> = data_encoding::HEXLOWER
                .decode(hex_str.as_bytes())
                .map_err(serde::de::Error::custom)?
                .into();

            Ok(EventContentRaw::from(bytes))
        } else {
            let bytes = Vec::<u8>::deserialize(de)?;
            Ok(EventContentRaw::from(bytes))
        }
    }
}

impl serde::Serialize for EventKind {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ser.serialize_u16(u16::from(*self))
    }
}

impl<'de> serde::de::Deserialize<'de> for EventKind {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(EventKind::from(u16::deserialize(de)?))
    }
}

#[derive(Debug, Snafu)]
pub enum ContentDeserializationError {
    #[snafu(transparent)]
    Validation {
        source: ContentValidationError,
    },
    #[snafu(transparent)]
    Decoding {
        source: cbor4ii::serde::DecodeError<Infallible>,
    },
    MissingContent,
}

pub type ContentDeserializationResult<T> = std::result::Result<T, ContentDeserializationError>;

impl EventContentUnsized {
    pub fn deserialize_cbor<T>(&self) -> ContentDeserializationResult<T>
    // pub fn deserialize_cbor<T>(&self) -> std::result::Result<T,
    // ciborium::de::Error<std::io::Error>>
    where
        T: EventContentKind,
    {
        // ciborium::from_reader(self.as_ref())
        let v: T = cbor4ii::serde::from_slice(self.as_ref())?;
        v.validate()?;

        Ok(v)
    }
}

impl VerifiedEventContent {
    pub fn deserialize_cbor<T>(&self) -> ContentDeserializationResult<T>
    // pub fn deserialize_cbor<T>(&self) -> std::result::Result<T,
    // ciborium::de::Error<std::io::Error>>
    where
        T: EventContentKind,
    {
        // ciborium::from_reader(self.as_ref())
        let Some(content) = self.content.as_ref() else {
            return MissingContentSnafu.fail();
        };
        let v: T = cbor4ii::serde::from_slice(content.as_slice())?;
        v.validate()?;

        Ok(v)
    }
}
#[cfg(test)]
mod tests;
