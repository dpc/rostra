#[macro_export]
macro_rules! array_type_define_min_max {
    (
        $(#[$outer:meta])*
        struct $t:tt, $n:literal
    ) => {
        $(#[$outer])*
        pub struct $t([u8; $n]);

        impl $t {

            pub const ZERO: Self = Self([0u8; $n]);
            pub const MAX: Self = Self([0xffu8; $n]);

            pub fn as_slice(&self) -> &[u8] {
                self.0.as_slice()
            }

            pub fn from_bytes(bytes: [u8; $n]) -> Self {
                Self(bytes)
            }

            pub fn to_bytes(self) -> [u8; $n] {
                self.0
            }
        }

        impl std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                <Self as std::fmt::Display>::fmt(self, f)
            }
        }
    }
}

#[macro_export]
macro_rules! array_type_define {
    (
        $(#[$outer:meta])*
        struct $t:tt, $n:literal
    ) => {
        $crate::array_type_define_min_max!(
            #[derive(Copy, Clone, Hash)]
            #[cfg_attr(feature = "bincode", derive(::bincode::Encode, ::bincode::Decode))]
            $(#[$outer])*
            struct $t, $n
        );
    }
}

#[macro_export]
macro_rules! array_type_define_public {
    (
        $(#[$outer:meta])*
        struct $t:tt, $n:literal
    ) => {
        $crate::array_type_define!(
            #[derive(PartialOrd, Ord, PartialEq, Eq)]
            $(#[$outer])*
            struct $t, $n
        );
    }
}

#[macro_export]
macro_rules! array_type_impl_serde {
    (
        struct $t:tt, $n:literal
    ) => {
        #[cfg(feature = "serde")]
        impl ::serde::Serialize for $t {
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

        #[cfg(feature = "serde")]
        impl<'de> ::serde::de::Deserialize<'de> for $t {
            fn deserialize<D>(d: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                if d.is_human_readable() {
                    let str = <String>::deserialize(d)?;
                    <Self as std::str::FromStr>::from_str(&str).map_err(|e| {
                        ::serde::de::Error::custom(format!("Deserialization error: {e:#}"))
                    })
                } else {
                    let bytes = <serde_bytes::ByteArray<$n>>::deserialize(d)?;
                    Ok(Self(bytes.into_array()))
                }
            }
        }
    };
}

#[macro_export]
macro_rules! array_type_impl_base32_str {
    (
        $t:tt
    ) => {
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

#[macro_export]
macro_rules! array_type_impl_base64_str {
    (
        $t:tt
    ) => {
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                data_encoding::BASE64_NOPAD.encode_write(self.as_slice(), f)
            }
        }

        impl std::str::FromStr for $t {
            type Err = data_encoding::DecodeError;

            fn from_str(s: &str) -> Result<$t, Self::Err> {
                let v = data_encoding::BASE64_NOPAD.decode(s.as_bytes())?;
                let a = v.try_into().map_err(|_| data_encoding::DecodeError {
                    position: 0,
                    kind: data_encoding::DecodeKind::Length,
                })?;
                Ok(Self(a))
            }
        }
    };
}

#[macro_export]
macro_rules! array_type_impl_zero_default {
    ($name:tt, $n:expr) => {
        impl Default for $name {
            fn default() -> Self {
                Self([0; $n])
            }
        }
    };
}
