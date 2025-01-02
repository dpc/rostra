use bincode::{Decode, Encode};
use rostra_core::id::RestRostraId;

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdRecord {
    pub id_rest: RestRostraId,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFolloweesRecord {
    pub persona: String,
}

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdsFolloweesTsRecord {
    pub ts: u64,
}
