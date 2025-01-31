use std::fmt::{self};
use std::str::FromStr;

use super::{ExternalEventId, RostraId, RostraIdSecretKey, RostraIdSecretKeyError};

impl fmt::Display for RostraId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&z32::encode(self.0.as_slice()))
    }
}

impl serde::Serialize for RostraId {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if s.is_human_readable() {
            s.serialize_str(&z32::encode(self.0.as_slice()))
        } else {
            s.serialize_bytes(&self.0)
        }
    }
}

impl<'de> serde::de::Deserialize<'de> for RostraId {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if d.is_human_readable() {
            let str = <String>::deserialize(d)?;
            let bytes = z32::decode(str.as_bytes())
                .map_err(|e| serde::de::Error::custom(format!("z32 deserialization error: {e}")))?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("Invalid length"));
            }
            Ok(Self(bytes.try_into().expect("Just checked length")))
        } else {
            let bytes = <Vec<u8>>::deserialize(d)?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("Invalid length"));
            }
            Ok(Self(bytes.try_into().expect("Just checked length")))
        }
    }
}

impl serde::Serialize for RostraIdSecretKey {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if s.is_human_readable() {
            s.serialize_str(&self.to_string())
        } else {
            s.serialize_bytes(&self.0)
        }
    }
}

impl<'de> serde::de::Deserialize<'de> for RostraIdSecretKey {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if d.is_human_readable() {
            let str = <String>::deserialize(d)?;
            let bytes = bip39::Mnemonic::from_str(&str)
                .map_err(|e| {
                    serde::de::Error::custom(format!("Mnemonic deserialization error: {e}"))
                })?
                .to_entropy();
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("Invalid length"));
            }
            Ok(Self(bytes.try_into().expect("Just checked length")))
        } else {
            let bytes = <Vec<u8>>::deserialize(d)?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("Invalid length"));
            }
            Ok(Self(bytes.try_into().expect("Just checked length")))
        }
    }
}
impl FromStr for RostraIdSecretKey {
    type Err = RostraIdSecretKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = bip39::Mnemonic::from_str(s)
            .map_err(|e| format!("Mnemonic deserialization error: {e}"))?
            .to_entropy();
        if bytes.len() != 32 {
            return Err(("Invalid length").to_string().into());
        }
        Ok(Self(bytes.try_into().expect("Just checked length")))
    }
}

// Add these to use FromStr/Display for serde:
impl serde::Serialize for ExternalEventId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for ExternalEventId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
