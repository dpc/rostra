use bincode::{Decode, Encode};
use events::EventsMissingRecord;
pub use events::{ContentState, EventRecord};
use ids::IdsFolloweesTsRecord;
pub use ids::{IdRecord, IdsFolloweesRecord};
use redb_bincode::TableDefinition;
use rostra_core::event::EventContent;
use rostra_core::id::{RostraId, ShortRostraId};
use rostra_core::ShortEventId;

pub(crate) mod events;
pub(crate) mod ids;

pub const TABLE_DB_VER: TableDefinition<'_, (), u64> = TableDefinition::new("db-ver");

pub const TABLE_SELF: TableDefinition<'_, (), ShortRostraId> = TableDefinition::new("self");

/// Basically `short_id` -> `full_id`, plus maybe more data in the future about
/// the id
pub const TABLE_IDS: TableDefinition<'_, ShortRostraId, IdRecord> = TableDefinition::new("ids");

/// Table with `who` -> `whom` following
pub const TABLE_IDS_FOLLOWEES: TableDefinition<'_, (ShortRostraId, RostraId), IdsFolloweesRecord> =
    TableDefinition::new("ids-followees");

pub const TABLE_IDS_FOLLOWEES_TS: TableDefinition<'_, ShortRostraId, IdsFolloweesTsRecord> =
    TableDefinition::new("ids-followees-ts");

pub const TABLE_EVENTS: TableDefinition<'_, ShortEventId, EventRecord> =
    TableDefinition::new("events");

pub const TABLE_EVENTS_CONTENT: TableDefinition<'_, ShortEventId, ContentState> =
    TableDefinition::new("events-content");

pub const TABLE_EVENTS_MISSING: TableDefinition<
    '_,
    (ShortRostraId, ShortEventId),
    EventsMissingRecord,
> = TableDefinition::new("events-missing");

#[derive(Decode, Encode, Debug)]
pub struct EventsHeadsTableValue;

pub const TABLE_EVENTS_HEADS: TableDefinition<
    '_,
    (ShortRostraId, ShortEventId),
    EventsHeadsTableValue,
> = TableDefinition::new("events-heads");
