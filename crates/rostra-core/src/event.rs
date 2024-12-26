use std::fmt;

macro_rules! impl_hash_type {
    ($t:tt) => {
        #[derive(Debug, Copy, Clone)]
        pub struct $t([u8; 32]);

        impl $t {
            pub fn as_slice(&self) -> &[u8] {
                self.0.as_slice()
            }
        }

        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                data_encoding::BASE32_NOPAD.encode_write(self.as_slice(), f)
            }
        }

        impl std::str::FromStr for $t {
            type Err = data_encoding::DecodeError;

            fn from_str(s: &str) -> Result<$t, Self::Err> {
                let v = data_encoding::BASE32_NOPAD.decode(s.as_bytes())?;
                let a = v.try_into().map_err(|_| data_encoding::DecodeError {
                    position: 0,
                    kind: data_encoding::DecodeKind::Length,
                })?;
                Ok(Self(a))
            }
        }
    };
}

impl_hash_type!(EventId);

pub struct Event {
    pub version: u8,
    pub parents: Vec<EventId>,
    pub kind: u16,
    pub content: Vec<u8>,
}
