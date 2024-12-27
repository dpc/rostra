use crate::{define_array_type, define_array_type_public};

#[cfg(feature = "ed25519-dalek")]
mod ed25519;

#[cfg(feature = "pkarr")]
mod pkarr;

define_array_type_public!(struct RostraId, 32);
define_array_type_public!(struct ShortRostraId, 16);
define_array_type!(struct RostraIdSecretKey, 32);
