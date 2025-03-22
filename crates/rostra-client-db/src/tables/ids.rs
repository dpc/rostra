use bincode::{Decode, Encode};
use rostra_core::Timestamp;
use rostra_core::event::{PersonaId, PersonaSelector};
use rostra_core::id::RestRostraId;

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdRecord {
    pub id_rest: RestRostraId,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFolloweesRecordV0 {
    pub ts: Timestamp,
    pub persona: PersonaId,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFolloweesRecord {
    pub ts: Timestamp,
    pub selector: Option<PersonaSelector>,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFollowersRecord {}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsUnfollowedRecord {
    pub ts: Timestamp,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsPersonaRecord {
    pub ts: u64,
    pub display_name: String,
}
