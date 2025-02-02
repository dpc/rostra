use std::str::FromStr;

use super::{ExternalEventId, RostraIdSecretKey, RostraIdSecretKeyError};

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
