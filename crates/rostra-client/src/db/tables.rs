use bincode::{Decode, Encode};
use events::EventsMissingRecord;
pub use events::{ContentState, EventRecord};
pub use ids::{IdRecord, IdsFolloweesRecord};
use ids::{IdsFollowersRecord, IdsPersonaRecord, IdsUnfollowedRecord};
use redb_bincode::TableDefinition;
use rostra_core::event::PersonaId;
use rostra_core::id::{RostraId, ShortRostraId};
use rostra_core::{ShortEventId, Timestamp};

pub(crate) mod events;
pub(crate) mod ids;

pub const TABLE_DB_VER: TableDefinition<'_, (), u64> = TableDefinition::new("db-ver");

pub const TABLE_ID_SELF: TableDefinition<'_, (), RostraId> = TableDefinition::new("id-self");

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

pub const TABLE_ID_PERSONAS: TableDefinition<'_, (RostraId, PersonaId), IdsPersonaRecord> =
    TableDefinition::new("personas");

pub const TABLE_EVENTS: TableDefinition<'_, ShortEventId, EventRecord> =
    TableDefinition::new("events");

pub const TABLE_EVENTS_BY_TIME: TableDefinition<'_, (Timestamp, ShortEventId), ()> =
    TableDefinition::new("events-by-time");

pub const TABLE_EVENTS_SELF: TableDefinition<'_, ShortEventId, ()> =
    TableDefinition::new("events-self");

pub const TABLE_EVENTS_CONTENT: TableDefinition<'_, ShortEventId, ContentState> =
    TableDefinition::new("events-content");

pub const TABLE_EVENTS_MISSING: TableDefinition<'_, (RostraId, ShortEventId), EventsMissingRecord> =
    TableDefinition::new("events-missing");

#[derive(Decode, Encode, Debug)]
pub struct EventsHeadsTableValue;

pub const TABLE_EVENTS_HEADS: TableDefinition<'_, (RostraId, ShortEventId), EventsHeadsTableValue> =
    TableDefinition::new("events-heads");
