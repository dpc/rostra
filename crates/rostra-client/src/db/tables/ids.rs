use bincode::{Decode, Encode};
use rostra_core::id::RestRostraId;

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdRecord {
    pub id_rest: RestRostraId,
}

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdFollowingRecord {
    something: bool,
}
