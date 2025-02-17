use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Deserializer};

pub(crate) fn empty_string_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    if let Some(str) = Option::<String>::deserialize(deserializer)? {
        let str = str.trim();
        if str.is_empty() {
            Ok(None)
        } else {
            T::deserialize(str.into_deserializer()).map(Some)
        }
    } else {
        Ok(None)
    }
}

#[allow(dead_code)]
pub(crate) fn trim_string<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let s = String::deserialize(deserializer)?;
    let s = s.trim();

    T::deserialize(s.into_deserializer())
}
