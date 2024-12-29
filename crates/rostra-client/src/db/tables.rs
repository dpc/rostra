use events::EventRecord;
use ids::IdRecord;
use redb_bincode::TableDefinition;
use rostra_core::id::ShortRostraId;
use rostra_core::ShortEventId;

mod events;
mod ids;

pub const TABLE_DB_VER: TableDefinition<'_, (), u64> = TableDefinition::new("db-ver");

pub const TABLE_SELF: TableDefinition<'_, (), ShortRostraId> = TableDefinition::new("self");

pub const TABLE_IDS: TableDefinition<'_, ShortRostraId, IdRecord> = TableDefinition::new("ids");

pub const TABLE_ID_SOCIAL_FOLLOWING: TableDefinition<'_, ShortRostraId, IdRecord> =
    TableDefinition::new("ids");

pub const TABLE_EVENTS: TableDefinition<'_, ShortEventId, EventRecord> =
    TableDefinition::new("events");

pub const TABLE_EVENTS_MISSING: TableDefinition<'_, (ShortRostraId, ShortEventId), ()> =
    TableDefinition::new("events_missing");

pub const TABLE_EVENTS_HEADS: TableDefinition<'_, (ShortRostraId, ShortEventId), ()> =
    TableDefinition::new("events_heads");
