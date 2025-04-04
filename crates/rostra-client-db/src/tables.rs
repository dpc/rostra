use bincode::{Decode, Encode};
pub use event::EventRecord;
use event::EventsMissingRecord;
use id_self::IdSelfAccountRecord;
use ids::{
    IdsFolloweesRecord, IdsFolloweesRecordV0, IdsFollowersRecord, IdsPersonaRecord,
    IdsUnfollowedRecord,
};
use rostra_core::event::{EventAuxKey, EventKind, IrohNodeId, PersonaId};
use rostra_core::id::{RestRostraId, RostraId, ShortRostraId};
use rostra_core::{ShortEventId, Timestamp};
use serde::Serialize;

pub use self::event::EventsHeadsTableRecord;
pub(crate) mod event;
pub(crate) mod id_self;
pub(crate) mod ids;

#[macro_export]
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
def_table!(ids_nodes: (RostraId, IrohNodeId) => IrohNodeRecord);
def_table!(ids_followees_v0: (RostraId, RostraId) => IdsFolloweesRecordV0);
def_table!(ids_followees: (RostraId, RostraId) => IdsFolloweesRecord);
def_table!(ids_followers: (RostraId, RostraId) => IdsFollowersRecord);
def_table!(ids_unfollowed: (RostraId, RostraId) => IdsUnfollowedRecord);
def_table!(ids_personas: (RostraId, PersonaId) => IdsPersonaRecord);

// EVENTS
def_table!(events: ShortEventId => EventRecord);
def_table!(events_singletons: (EventKind, EventAuxKey) => Latest<event::EventSingletonRecord>);
def_table!(events_missing: (RostraId, ShortEventId) => EventsMissingRecord);
def_table!(events_heads: (RostraId, ShortEventId) => EventsHeadsTableRecord);
def_table!(events_self: ShortEventId => ());
def_table!(events_content: ShortEventId => event::EventContentStateOwned);
def_table!(events_content_missing: ShortEventId => ());
def_table!(events_by_time: (Timestamp, ShortEventId) => ());

// SOCIAL
def_table!(social_profiles_v0: RostraId => Latest<IdSocialProfileRecordV0>);
def_table!(social_profiles: RostraId => Latest<IdSocialProfileRecord>);
def_table!(social_posts_v0: (ShortEventId)=> SocialPostRecordV0);
def_table!(social_posts: (ShortEventId)=> SocialPostRecord);
def_table!(social_posts_replies: (ShortEventId, Timestamp, ShortEventId)=> SocialPostsRepliesRecord);
def_table!(social_posts_reactions: (ShortEventId, Timestamp, ShortEventId)=> SocialPostsReactionsRecord);
def_table!(social_posts_by_time: (Timestamp, ShortEventId) => ());

#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub struct Latest<T> {
    pub ts: Timestamp,
    pub inner: T,
}

#[derive(Debug, Encode, Serialize, Decode, Clone, Copy)]
pub struct SocialPostsRepliesRecord;
#[derive(Debug, Encode, Serialize, Decode, Clone, Copy)]
pub struct SocialPostsReactionsRecord;

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdSocialProfileRecordV0 {
    pub event_id: ShortEventId,
    pub display_name: String,
    pub bio: String,
    pub img_mime: String,
    pub img: Vec<u8>,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IdSocialProfileRecord {
    pub event_id: ShortEventId,
    pub display_name: String,
    pub bio: String,
    pub avatar: Option<(String, Vec<u8>)>,
}

#[derive(
    Debug,
    Encode,
    Decode,
    Clone,
    // Note: needs to be default so we can track number of replies even before we get what was
    // replied to
    Default,
)]
pub struct SocialPostRecordV0 {
    pub reply_count: u64,
}

#[derive(
    Debug,
    Encode,
    Decode,
    Serialize,
    Clone,
    // Note: needs to be default so we can track number of replies even before we get what was
    // replied to
    Default,
)]
pub struct SocialPostRecord {
    pub reply_count: u64,
    pub reaction_count: u64,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct IrohNodeRecord {
    pub announcement_ts: Timestamp,
    pub stats: IrohNodeStats,
}
#[derive(Debug, Encode, Decode, Clone, Default)]
pub struct IrohNodeStats {
    pub last_success: Option<Timestamp>,
    pub last_failure: Option<Timestamp>,
    pub success_count: u64,
    pub fail_count: u64,
}
