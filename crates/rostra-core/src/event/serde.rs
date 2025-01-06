use super::{EventKind, EventSignature};

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

#[cfg(test)]
mod tests;
