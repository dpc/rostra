use event::EventsMissingRecord;
pub use event::{ContentState, EventRecord};
use id_self::IdSelfRecord;
pub use ids::IdsFolloweesRecord;
use ids::{IdsFollowersRecord, IdsPersonaRecord, IdsUnfollowedRecord};
use rostra_core::event::PersonaId;
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};

pub use self::event::EventsHeadsTableRecord;
pub(crate) mod event;
pub(crate) mod id_self;
pub(crate) mod ids;

macro_rules! def_table {
    ($name:ident : $k:tt => $v:tt) => {
        #[allow(unused)]
        pub mod $name {
            use super::*;
            pub type Key = $k;
            pub type Value = $v;
            pub type Definition<'a> = redb_bincode::TableDefinition<'a, Key, Value>;
            pub trait ReadableTable: redb_bincode::ReadableTable<Key, Value> {}
            impl<RT> ReadableTable for RT where RT: redb_bincode::ReadableTable<Key, Value> {}
            pub type Table<'a> = redb_bincode::Table<'a, Key, Value>;
            pub const TABLE: Definition = redb_bincode::TableDefinition::new(stringify!($module));
        }
    };
}

def_table!(db_migration_ver: () => u64);
def_table!(ids_self: () => IdSelfRecord);
def_table!(ids_followees: (RostraId, RostraId) => IdsFolloweesRecord);
def_table!(ids_followers: (RostraId, RostraId) => IdsFollowersRecord);
def_table!(ids_unfollowed: (RostraId, RostraId) => IdsUnfollowedRecord);
def_table!(ids_personas: (RostraId, PersonaId) => IdsPersonaRecord);
def_table!(events_by_time: (Timestamp, ShortEventId) => ());
def_table!(events: ShortEventId => EventRecord);
def_table!(events_content: ShortEventId => ContentState);
def_table!(events_self: ShortEventId => ());
def_table!(events_missing: (RostraId, ShortEventId) => EventsMissingRecord);
def_table!(events_heads: (RostraId, ShortEventId) => EventsHeadsTableRecord);
