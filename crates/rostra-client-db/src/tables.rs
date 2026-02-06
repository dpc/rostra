//! Database table definitions for the Rostra client.
//!
//! # Data Model Overview
//!
//! The database stores a local view of the distributed event DAG that forms the
//! Rostra social network. All data in Rostra propagates as cryptographically
//! signed [`Event`]s, which form a DAG structure where each event references
//! parent events.
//!
//! ## Key Concepts
//!
//! - **Event**: A signed, immutable unit of data. Events form a DAG where each
//!   event references one or two parent events.
//! - **Event Content**: The payload of an event, stored separately for
//!   efficiency. Content can be pruned while keeping the event structure.
//! - **RostraId**: A user's public identity (derived from their Ed25519 public
//!   key).
//! - **ShortEventId**: A truncated event ID used for storage efficiency.
//! - **Singleton Events**: Events where only the latest instance matters (e.g.,
//!   profile updates).
//! - **Head Events**: Events with no known children - the current "tips" of the
//!   DAG for an identity.
//!
//! ## Table Categories
//!
//! ### Identity Tables (`ids_*`)
//! Store information about identities (users) and their relationships.
//!
//! ### Event Tables (`events_*`)
//! Store the event DAG structure, content, and various indices.
//!
//! ### Social Tables (`social_*`)
//! Store derived social data extracted from events (profiles, posts, etc.).
//!
//! [`Event`]: rostra_core::event::Event

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
use rostra_core::{ContentHash, ShortEventId, Timestamp};
use serde::Serialize;

pub use self::event::{
    ContentStoreRecordOwned, EventContentResult, EventContentStateNew, EventReceivedRecord,
    EventReceivedSource, EventsHeadsTableRecord,
};
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
// ============================================================================
// SYSTEM TABLES
// ============================================================================

def_table! {
    /// Tracks database/schema version for migrations.
    db_version: () => u64
}

// ============================================================================
// IDENTITY TABLES
// ============================================================================

def_table! {
    /// Information about the local user's own account.
    ///
    /// Contains the user's RostraId and the secret key for their iroh network
    /// identity.
    ids_self: () => IdSelfAccountRecord
}

def_table! {
    /// Mapping from shortened to full `RostraId`.
    ///
    /// We use [`ShortRostraId`] in high-volume tables where an extra lookup
    /// to reconstruct the full [`RostraId`] is acceptable, to save space.
    ids_full: ShortRostraId => RestRostraId
}

def_table! {
    /// Known network endpoints (iroh nodes) for each identity.
    ///
    /// Key: (identity, node_id)
    /// Used to discover how to connect to peers for syncing.
    ids_nodes: (RostraId, IrohNodeId) => IrohNodeRecord
}

// Legacy table, kept for migrations
def_table!(ids_followees_v0: (RostraId, RostraId) => IdsFolloweesRecordV0);

def_table! {
    /// Who each identity follows.
    ///
    /// Key: (follower, followee)
    /// Value: timestamp and persona selector (which personas to see from followee)
    ///
    /// Note: `selector = None` means "pending unfollow" - the entry exists to
    /// track the unfollow timestamp but the follow relationship is inactive.
    ids_followees: (RostraId, RostraId) => IdsFolloweesRecord
}

def_table! {
    /// Who follows each identity (reverse index of `ids_followees`).
    ///
    /// Key: (followee, follower)
    /// Used to quickly find all followers of an identity.
    ids_followers: (RostraId, RostraId) => IdsFollowersRecord
}

def_table! {
    /// Tracks unfollows with timestamps.
    ///
    /// Key: (unfollower, unfollowee)
    /// Used to prevent reprocessing old follow events that predate an unfollow.
    ids_unfollowed: (RostraId, RostraId) => IdsUnfollowedRecord
}

def_table! {
    /// Custom personas defined by users.
    ///
    /// Key: (identity, persona_id)
    /// Personas allow users to segment their posts (e.g., personal vs
    /// professional).
    ids_personas: (RostraId, PersonaId) => IdsPersonaRecord
}

def_table! {
    /// Aggregate data usage per identity.
    ///
    /// Tracks the total storage used by each identity's events and content.
    /// Updated incrementally as events are added and content state changes.
    ids_data_usage: RostraId => IdsDataUsageRecord
}

/// Aggregate data usage record for an identity.
///
/// Tracks both event metadata size (fixed per event) and content size
/// (variable, based on actual content stored).
#[derive(Debug, Encode, Decode, Clone, Copy, Default, Serialize)]
pub struct IdsDataUsageRecord {
    /// Total size of event metadata (envelopes) in bytes.
    ///
    /// Each event contributes a fixed size (Event struct + signature = 192
    /// bytes).
    pub metadata_size: u64,

    /// Total size of content (payloads) in bytes.
    ///
    /// Only content in the `Available` state is counted. Pruned/Deleted content
    /// is not included (as it doesn't consume storage).
    pub content_size: u64,
}

// ============================================================================
// EVENT TABLES
// ============================================================================

def_table! {
    /// Main event storage - the signed events forming the DAG.
    ///
    /// This is the authoritative record of events we've received and verified.
    events: ShortEventId => EventRecord
}

// TODO: this version is stupid, make a migration that deletes it and renames
// `events_singletons_new` to old name
def_table! {
    /// Singleton events - events where only the latest matters (DEPRECATED).
    ///
    /// Key: (event_kind, aux_key)
    /// For events like profile updates where we only care about the latest
    /// version. This table doesn't distinguish by author - see
    /// `events_singletons_new`.
    events_singletons: (EventKind, EventAuxKey) => Latest<event::EventSingletonRecord>
}

def_table! {
    /// Singleton events indexed by author (preferred over `events_singletons`).
    ///
    /// Key: (author, event_kind, aux_key)
    /// Tracks the latest singleton event per author/kind/aux_key combination.
    events_singletons_new: (RostraId, EventKind, EventAuxKey) => Latest<event::EventSingletonRecord>
}

def_table! {
    /// Events we know about but haven't received yet.
    ///
    /// Key: (author, event_id)
    /// When we receive an event that references a parent we don't have, we
    /// record it here. This drives the sync protocol to fetch missing events.
    events_missing: (RostraId, ShortEventId) => EventsMissingRecord
}

def_table! {
    /// Current DAG heads per identity.
    ///
    /// Key: (author, event_id)
    /// "Head" events are events with no known children - the current tips of
    /// the DAG. Used for sync protocol and to determine where to append new
    /// events.
    events_heads: (RostraId, ShortEventId) => EventsHeadsTableRecord
}

def_table! {
    /// Index of the local user's own events.
    ///
    /// Used for efficient random access to own events (e.g., for verification
    /// or export).
    events_self: ShortEventId => ()
}

def_table! {
    /// Event content storage (the actual payloads).
    ///
    /// Stored separately from event metadata so content can be pruned while
    /// keeping the DAG structure intact. Content can be in various states:
    /// Present, Deleted, Pruned, or Invalid.
    events_content: ShortEventId => event::EventContentStateOwned
}

def_table! {
    /// Reference count for event content (DEPRECATED - use content_rc).
    ///
    /// Tracks how many references exist to each piece of content. When count
    /// reaches zero, content can be garbage collected.
    events_content_rc_count: ShortEventId => u64
}

// ============================================================================
// CONTENT DEDUPLICATION TABLES (V1)
// These tables store content by hash for deduplication across events.
// ============================================================================

def_table! {
    /// Content store - stores content by its hash for deduplication.
    ///
    /// Key: ContentHash (blake3 hash of the content)
    /// Value: The actual content
    ///
    /// This enables identical content (e.g., same image posted by multiple
    /// users) to be stored only once.
    content_store: ContentHash => ContentStoreRecordOwned
}

def_table! {
    /// Reference count for content by hash.
    ///
    /// Key: ContentHash
    /// Value: Number of events referencing this content
    ///
    /// When count reaches zero, content can be garbage collected from
    /// content_store.
    content_rc: ContentHash => u64
}

def_table! {
    /// Per-event content state (replaces events_content for new events).
    ///
    /// Key: ShortEventId
    /// Value: State of content for this event (Available, Deleted, Pruned)
    ///
    /// Unlike events_content which stored content inline, this just tracks
    /// state. The actual content is in content_store, looked up via the
    /// event's content_hash field.
    events_content_state: ShortEventId => EventContentStateNew
}

def_table! {
    /// Events whose content we want but don't have yet.
    ///
    /// When we receive an event but not its content, we record it here.
    /// The sync protocol uses this to request missing content from peers.
    events_content_missing: ShortEventId => ()
}

def_table! {
    /// Time-ordered index of all events.
    ///
    /// Key: (timestamp, event_id)
    /// Used for time-based queries and pagination across all events.
    events_by_time: (Timestamp, ShortEventId) => ()
}

def_table! {
    /// Tracks when and how we received each event.
    ///
    /// Key: (received_timestamp, event_id)
    /// Value: Information about the reception source (push/pull, from whom, etc.)
    ///
    /// This enables tracking network propagation delays (by comparing received
    /// timestamp vs event's author timestamp), debugging sync issues, and
    /// analytics about event acquisition patterns.
    events_received_at: (Timestamp, ShortEventId) => EventReceivedRecord
}

// ============================================================================
// SOCIAL TABLES
// Derived data extracted from social-related events for efficient querying.
// ============================================================================

// Legacy table, kept for migrations
def_table!(social_profiles_v0: RostraId => Latest<IdSocialProfileRecordV0>);

def_table! {
    /// User profile information (display name, bio, avatar).
    ///
    /// Extracted from SOCIAL_PROFILE_UPDATE events. Only the latest profile
    /// per user is stored.
    social_profiles: RostraId => Latest<IdSocialProfileRecord>
}

// Legacy table, kept for migrations
def_table!(social_posts_v0: (ShortEventId)=> SocialPostRecordV0);

def_table! {
    /// Post metadata (reply and reaction counts).
    ///
    /// This table stores aggregate counts for posts. The actual post content
    /// is in `events_content`. A record may exist here even before we receive
    /// the post itself (to track reply counts from replies we've seen).
    social_posts: (ShortEventId)=> SocialPostRecord
}

def_table! {
    /// Index of replies to posts.
    ///
    /// Key: (parent_post_id, reply_timestamp, reply_event_id)
    /// Enables efficient retrieval of all replies to a post, ordered by time.
    social_posts_replies: (ShortEventId, Timestamp, ShortEventId)=> SocialPostsRepliesRecord
}

def_table! {
    /// Index of reactions to posts.
    ///
    /// Key: (parent_post_id, reaction_timestamp, reaction_event_id)
    /// Enables efficient retrieval of all reactions to a post, ordered by time.
    social_posts_reactions: (ShortEventId, Timestamp, ShortEventId)=> SocialPostsReactionsRecord
}

def_table! {
    /// Time-ordered index of social posts.
    ///
    /// Key: (timestamp, post_event_id)
    /// Used for timeline queries - fetching posts in chronological order.
    social_posts_by_time: (Timestamp, ShortEventId) => ()
}

/// Wrapper for values where only the latest version matters.
///
/// Used for singleton-style data where we track timestamps to ensure
/// we only keep the most recent value (e.g., profile updates).
#[derive(Debug, Encode, Decode, Clone, Serialize)]
pub struct Latest<T> {
    /// Timestamp when this value was created/updated
    pub ts: Timestamp,
    /// The actual value
    pub inner: T,
}

/// Marker record for the `social_posts_replies` index.
///
/// The key `(parent_post_id, timestamp, reply_id)` contains all needed info;
/// this empty struct just marks the entry's existence.
#[derive(Debug, Encode, Serialize, Decode, Clone, Copy)]
pub struct SocialPostsRepliesRecord;

/// Marker record for the `social_posts_reactions` index.
///
/// The key `(parent_post_id, timestamp, reaction_id)` contains all needed info;
/// this empty struct just marks the entry's existence.
#[derive(Debug, Encode, Serialize, Decode, Clone, Copy)]
pub struct SocialPostsReactionsRecord;

/// Legacy profile record format (V0).
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdSocialProfileRecordV0 {
    pub event_id: ShortEventId,
    pub display_name: String,
    pub bio: String,
    pub img_mime: String,
    pub img: Vec<u8>,
}

/// User profile information extracted from SOCIAL_PROFILE_UPDATE events.
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdSocialProfileRecord {
    /// The event ID that this profile data came from
    pub event_id: ShortEventId,
    /// User's display name
    pub display_name: String,
    /// User's biography/description
    pub bio: String,
    /// Avatar image: (mime_type, image_bytes)
    pub avatar: Option<(String, Vec<u8>)>,
}

/// Legacy post record format (V0).
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

/// Aggregate metadata for a social post.
///
/// Note: This record may exist before we receive the actual post content,
/// because we increment reply/reaction counts when we see replies to a post
/// we haven't received yet.
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
    /// Number of replies to this post
    pub reply_count: u64,
    /// Number of reactions to this post
    pub reaction_count: u64,
}

/// Information about an iroh network endpoint for an identity.
#[derive(Debug, Encode, Decode, Clone)]
pub struct IrohNodeRecord {
    /// When this node endpoint was announced
    pub announcement_ts: Timestamp,
    /// Connection statistics for this endpoint
    pub stats: IrohNodeStats,
}

/// Connection statistics for an iroh node endpoint.
///
/// Used to track reliability of endpoints for prioritizing connection attempts.
#[derive(Debug, Encode, Decode, Clone, Default)]
pub struct IrohNodeStats {
    /// Last successful connection time
    pub last_success: Option<Timestamp>,
    /// Last failed connection time
    pub last_failure: Option<Timestamp>,
    /// Total successful connection count
    pub success_count: u64,
    /// Total failed connection count
    pub fail_count: u64,
}
