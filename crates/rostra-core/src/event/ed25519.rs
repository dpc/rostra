use ed25519_dalek::ed25519::signature::SignerMut as _;

use super::Event;
use crate::bincode::STD_BINCODE_CONFIG;
use crate::id::RostraIdSecretKey;

impl Event {
    pub fn sign_by(&self, secret_key: RostraIdSecretKey) -> ed25519_dalek::Signature {
        let encoded =
            bincode::encode_to_vec(self, STD_BINCODE_CONFIG).expect("Can't fail to encode");

        ed25519_dalek::SigningKey::from(secret_key).sign(&encoded)
    }
}
