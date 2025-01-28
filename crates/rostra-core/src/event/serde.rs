use std::io;
use std::sync::Arc;

use super::{EventContent, EventContentUnsized, EventKind, EventSignature};

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

impl serde::Serialize for EventContent {
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

impl<'de> serde::de::Deserialize<'de> for EventContent {
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

            Ok(EventContent::from(bytes))
        } else {
            let bytes = Vec::<u8>::deserialize(de)?;
            Ok(EventContent::from(bytes))
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

impl EventContentUnsized {
    pub fn deserialize_cbor<T>(&self) -> Result<T, ::ciborium::de::Error<io::Error>>
    where
        T: ::serde::de::DeserializeOwned,
    {
        ciborium::from_reader(&mut self.as_ref())
    }
}

#[cfg(test)]
mod tests;
