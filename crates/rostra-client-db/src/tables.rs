use bincode::{Decode, Encode};
pub use event::EventRecord;
use event::EventsMissingRecord;
use id_self::IdSelfAccountRecord;
use ids::{IdsFolloweesRecord, IdsFollowersRecord, IdsPersonaRecord, IdsUnfollowedRecord};
use rostra_core::event::PersonaId;
use rostra_core::id::{RestRostraId, RostraId, ShortRostraId};
use rostra_core::{ShortEventId, Timestamp};

pub use self::event::EventsHeadsTableRecord;
pub(crate) mod event;
pub(crate) mod id_self;
pub(crate) mod ids;

macro_rules! def_table {
    ($(#[$outer:meta])*
        $name:ident : $k:ty => $v:ty) => {
        #[allow(unused)]
        $(#[$outer])*
        pub mod $name {
            use super::*;
            pub type Key = $k;
            pub type Value = $v;
            pub type Definition<'a> = redb_bincode::TableDefinition<'a, Key, Value>;
            pub trait ReadableTable: redb_bincode::ReadableTable<Key, Value> {}
            impl<RT> ReadableTable for RT where RT: redb_bincode::ReadableTable<Key, Value> {}
            pub type Table<'a> = redb_bincode::Table<'a, Key, Value>;
            pub const TABLE: Definition = redb_bincode::TableDefinition::new(stringify!($name));
        }
    };
}
def_table! {
    /// Tracks database/schema version
    db_version: () => u64
}

def_table! {
    /// Information about own account
    ids_self: () => IdSelfAccountRecord
}

def_table! {
    /// Mapping from shorttened to full `RostraId`
    ///
    /// We use [`ShortRostraId`] in the most massive tables, where extra lookup
    /// to a full [`RostraId`] doesn't matter, to save space.
    ids_full: ShortRostraId => RestRostraId
}
def_table!(ids_followees: (RostraId, RostraId) => IdsFolloweesRecord);
def_table!(ids_followers: (RostraId, RostraId) => IdsFollowersRecord);
def_table!(ids_unfollowed: (RostraId, RostraId) => IdsUnfollowedRecord);
def_table!(ids_personas: (RostraId, PersonaId) => IdsPersonaRecord);

// EVENTS
def_table!(events: ShortEventId => EventRecord);
def_table!(events_missing: (RostraId, ShortEventId) => EventsMissingRecord);
def_table!(events_heads: (RostraId, ShortEventId) => EventsHeadsTableRecord);
def_table!(events_self: ShortEventId => ());
def_table!(events_content: ShortEventId => event::EventContentStateOwned);
def_table!(events_by_time: (Timestamp, ShortEventId) => ());

// SOCIAL
def_table!(social_profile: RostraId => Latest<IdSocialProfileRecord>);
def_table!(social_comment: (ShortEventId, Timestamp, ShortEventId)=> ());

#[derive(Debug, Encode, Decode, Clone)]
pub struct Latest<T> {
    pub ts: Timestamp,
    pub inner: T,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdSocialProfileRecord {
    pub event_id: ShortEventId,
    pub display_name: String,
    pub bio: String,
    pub img_mime: String,
    pub img: Vec<u8>,
}
