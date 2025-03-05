use serde::Serialize;

use super::EventContentUnsized;

impl Serialize for EventContentUnsized {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: ::serde::Serializer,
    {
        if s.is_human_readable() {
            s.serialize_str(&self.to_string())
        } else {
            s.serialize_bytes(&self.0)
        }
    }
}
