use bincode::{Decode, Encode};
use event::EventsMissingRecord;
pub use event::{ContentState, EventRecord};
use id_self::IdSelfRecord;
pub use ids::{IdRecord, IdsFolloweesRecord};
use ids::{IdsFollowersRecord, IdsPersonaRecord, IdsUnfollowedRecord};
use redb_bincode::TableDefinition;
use rostra_core::event::PersonaId;
use rostra_core::id::{RostraId, ShortRostraId};
use rostra_core::{ShortEventId, Timestamp};

pub(crate) mod event;
pub(crate) mod id_self;
pub(crate) mod ids;

pub const TABLE_DB_VER: TableDefinition<'_, (), u64> = TableDefinition::new("db-ver");

pub const TABLE_ID_SELF: TableDefinition<'_, (), IdSelfRecord> = TableDefinition::new("id-self");

/// Basically `short_id` -> `full_id`, plus maybe more data in the future about
/// the id
pub const TABLE_IDS: TableDefinition<'_, ShortRostraId, IdRecord> = TableDefinition::new("ids");

/// Table with `who` -> `whom` following
pub const TABLE_ID_FOLLOWEES: TableDefinition<'_, (RostraId, RostraId), IdsFolloweesRecord> =
    TableDefinition::new("ids-followees");

pub const TABLE_ID_FOLLOWERS: TableDefinition<'_, (RostraId, RostraId), IdsFollowersRecord> =
    TableDefinition::new("ids-followers");
pub const TABLE_ID_UNFOLLOWED: TableDefinition<'_, (RostraId, RostraId), IdsUnfollowedRecord> =
    TableDefinition::new("ids-unfollowed");

pub type TableIdPersonas<'a> = TableDefinition<'a, (RostraId, PersonaId), IdsPersonaRecord>;
pub const TABLE_ID_PERSONAS: TableIdPersonas = TableDefinition::new("personas");

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

def_table!(events_by_time: (Timestamp, ShortEventId) => ());
def_table!(events: ShortEventId => EventRecord);
def_table!(events_content: ShortEventId => ContentState);

pub const TABLE_EVENTS_SELF: TableDefinition<'_, ShortEventId, ()> =
    TableDefinition::new("events-self");

pub const TABLE_EVENTS_MISSING: TableDefinition<'_, (RostraId, ShortEventId), EventsMissingRecord> =
    TableDefinition::new("events-missing");

#[derive(Decode, Encode, Debug)]
pub struct EventsHeadsTableValue;

pub const TABLE_EVENTS_HEADS: TableDefinition<'_, (RostraId, ShortEventId), EventsHeadsTableValue> =
    TableDefinition::new("events-heads");
