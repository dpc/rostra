use ed25519_dalek::ed25519::signature::SignerMut as _;
use ed25519_dalek::{SignatureError, VerifyingKey};

use super::{Event, EventSignature, SignedEvent};
use crate::bincode::STD_BINCODE_CONFIG;
use crate::id::{RostraId, RostraIdSecretKey};

impl Event {
    pub fn sign_by(&self, secret_key: RostraIdSecretKey) -> EventSignature {
        let encoded =
            bincode::encode_to_vec(self, STD_BINCODE_CONFIG).expect("Can't fail to encode");

        ed25519_dalek::SigningKey::from(secret_key)
            .sign(&encoded)
            .into()
    }

    pub fn signed_by(self, secret_key: RostraIdSecretKey) -> SignedEvent {
        let sig = self.sign_by(secret_key);
        SignedEvent { event: self, sig }
    }

    pub fn verified_signed_by(
        &self,
        sig: EventSignature,
        id: RostraId,
    ) -> Result<(), SignatureError> {
        let encoded =
            bincode::encode_to_vec(self, STD_BINCODE_CONFIG).expect("Can't fail to encode");

        Self::verified_signed_by_raw(&encoded, sig, id)
    }

    pub fn verified_signed_by_raw(
        bytes: &[u8],
        sig: EventSignature,
        id: RostraId,
    ) -> Result<(), SignatureError> {
        VerifyingKey::from(id).verify_strict(bytes, &sig.into())
    }
}

impl From<ed25519_dalek::Signature> for EventSignature {
    fn from(value: ed25519_dalek::Signature) -> Self {
        Self(value.to_bytes())
    }
}

impl From<EventSignature> for ed25519_dalek::Signature {
    fn from(value: EventSignature) -> Self {
        ed25519_dalek::Signature::from_bytes(&value.0)
    }
}
