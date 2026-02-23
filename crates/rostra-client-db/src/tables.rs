//! Database table definitions for the Rostra client.
//!
//! For detailed documentation on content lifecycle, state transitions, and
//! edge cases, see `docs/content-lifecycle.md` in this crate.
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
//! ## Content Storage Model
//!
//! Content is stored separately from event metadata for deduplication and
//! efficient pruning:
//!
//! - **[`content_store`]**: Stores actual content by [`ContentHash`] (blake3).
//!   Identical content is stored only once.
//! - **[`content_rc`]**: Reference count per content hash. Managed at event
//!   insertion time (not when content arrives).
//! - **[`events_content_state`]**: Tracks per-event content processing state.
//!   See "Content Lifecycle" below for details.
//! - **[`events_content_missing`]**: Events whose content bytes we want but
//!   don't have yet in [`content_store`].
//!
//! ### Content Lifecycle
//!
//! The key insight is that **event insertion** and **content processing** are
//! separate operations. An event can be inserted before we have its content,
//! and the same content can arrive multiple times (for different events).
//!
//! **Event Insertion** (via `insert_event_tx`):
//! 1. Event is added to [`events`] table
//! 2. RC is incremented in [`content_rc`] (for all non-deleted events,
//!    including `content_len == 0`)
//! 3. If `content_len > 0`: event is marked as
//!    [`Missing`](EventContentState::Missing) in [`events_content_state`], and
//!    if content bytes are not in [`content_store`], event is added to
//!    [`events_content_missing`]
//! 4. If `content_len == 0`: empty content stored in [`content_store`], event
//!    goes straight to "processed" (no entry in [`events_content_state`])
//!
//! **Content Processing** (via `process_event_content_tx`):
//! 1. Check if event is `Missing` (if not, skip - already processed)
//! 2. Process content side effects (e.g., increment reply counts, update
//!    follows)
//! 3. Store content bytes in [`content_store`] if not already there
//! 4. Remove event from [`events_content_missing`] (if present)
//! 5. Remove `Missing` marker from [`events_content_state`]
//!
//! **Content Deletion** (author requests content deletion):
//! 1. Event's content state changes to [`Deleted`](EventContentState::Deleted)
//!    in [`events_content_state`]
//! 2. RC is decremented in [`content_rc`]
//!
//! **Content Pruning** (local decision, e.g., content too large):
//! 1. Event's content state changes to [`Pruned`](EventContentState::Pruned) in
//!    [`events_content_state`]
//! 2. RC is decremented in [`content_rc`]
//!
//! **Garbage Collection**:
//! When RC reaches 0, content is removed from [`content_store`].
//!
//! ### Interpreting `events_content_state`
//!
//! - **No entry**: Content has been processed (or `content_len == 0`)
//! - **`Missing`**: Event inserted but content not yet received/processed
//! - **`Invalid`**: Content failed validation (e.g. CBOR deserialization)
//! - **`Deleted`**: Content deleted by author
//! - **`Pruned`**: Content pruned locally
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
//! [`ContentHash`]: rostra_core::ContentHash

use bincode::{Decode, Encode};
pub use event::EventRecord;
use event::EventsMissingRecord;
use id_self::IdSelfAccountRecord;
use ids::{IdsFolloweesRecord, IdsFollowersRecord, IdsPersonaRecord, IdsUnfollowedRecord};
use rostra_core::event::{EventAuxKey, EventKind, IrohNodeId, PersonaId};
use rostra_core::id::{RestRostraId, RostraId, ShortRostraId};
use rostra_core::{ContentHash, ShortEventId, Timestamp};
use serde::Serialize;

pub use self::event::{
    ContentStoreRecordOwned, EventContentResult, EventContentState, EventReceivedRecord,
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
/// Tracks event metadata and content/payload sizes and counts,
/// broken down by lifecycle state (current, missing, deleted, pruned, invalid).
///
/// **Invariants:**
///
/// - `current_metadata_{size,num} == total_metadata_{size,num}` (until event
///   pruning exists)
/// - `total_content_size == current_content_size + deleted_payload_size +
///   pruned_payload_size + missing_payload_size + invalid_payload_size`
/// - `total_payload_num == current_payload_num + deleted_payload_num +
///   pruned_payload_num + missing_payload_num + invalid_payload_num`
#[derive(Debug, Encode, Decode, Clone, Copy, Default, Serialize)]
pub struct IdsDataUsageRecord {
    // -- Metadata (event envelopes) --
    /// Size of event metadata currently stored, in bytes.
    /// Each event contributes 192 bytes (Event struct + signature).
    pub current_metadata_size: u64,

    /// Total metadata size of all events we know about, in bytes.
    /// Same as `current_metadata_size` until event pruning is implemented.
    pub total_metadata_size: u64,

    /// Number of events currently stored.
    pub current_metadata_num: u64,

    /// Total number of events we know about.
    /// Same as `current_metadata_num` until event pruning is implemented.
    pub total_metadata_num: u64,

    // -- Content/Payloads --
    /// Size of payload data currently stored and processed, in bytes.
    pub current_content_size: u64,

    /// Total payload size of all events we know about, in bytes.
    /// Includes current + missing + deleted + pruned + invalid.
    pub total_content_size: u64,

    /// Number of payloads currently stored and processed.
    pub current_payload_num: u64,

    /// Total number of payloads we know about.
    pub total_payload_num: u64,

    /// Size of payloads not yet received/processed, in bytes.
    pub missing_payload_size: u64,

    /// Number of payloads not yet received/processed.
    pub missing_payload_num: u64,

    /// Size of payloads deleted by their author, in bytes.
    pub deleted_payload_size: u64,

    /// Number of payloads deleted by their author.
    pub deleted_payload_num: u64,

    /// Size of payloads pruned locally, in bytes.
    pub pruned_payload_size: u64,

    /// Number of payloads pruned locally.
    pub pruned_payload_num: u64,

    /// Size of payloads that failed content validation, in bytes.
    pub invalid_payload_size: u64,

    /// Number of payloads that failed content validation.
    pub invalid_payload_num: u64,
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

def_table! {
    /// Singleton events - events where only the latest matters.
    ///
    /// Key: (author, event_kind, aux_key)
    /// For events like profile updates where we only care about the latest
    /// version per author/kind/aux_key combination.
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

// ============================================================================
// CONTENT STORAGE TABLES
// These tables implement content deduplication and state tracking.
// ============================================================================

def_table! {
    /// Content store - stores content by its hash for deduplication.
    ///
    /// Key: ContentHash (blake3 hash of the content)
    /// Value: The actual content bytes
    ///
    /// This enables identical content (e.g., same image posted by multiple
    /// users) to be stored only once. Content is removed when its reference
    /// count in `content_rc` reaches zero.
    content_store: ContentHash => ContentStoreRecordOwned
}

def_table! {
    /// Reference count for content by hash.
    ///
    /// Key: ContentHash
    /// Value: Number of events referencing this content
    ///
    /// **Important**: RC is managed at event insertion time, not when content
    /// arrives. When an event is inserted, its content_hash RC is incremented
    /// (unless the event is already deleted/pruned). When content is deleted
    /// or pruned, RC is decremented. When RC reaches zero, content can be
    /// garbage collected from `content_store`.
    content_rc: ContentHash => u64
}

def_table! {
    /// Per-event content processing state.
    ///
    /// Key: ShortEventId
    /// Value: [`EventContentState`] (Missing, Deleted, Pruned, or Invalid)
    ///
    /// This table tracks the content processing state for each event:
    ///
    /// - **No entry**: Content has been processed (side effects applied), or
    ///   the event has `content_len == 0`. This is the normal state.
    /// - **`Missing`**: Event was inserted but content hasn't been received or
    ///   processed yet. Content side effects (reply counts, follow updates,
    ///   etc.) have not been applied.
    /// - **`Deleted`**: Content was deleted by the author via a deletion event.
    /// - **`Pruned`**: Content was pruned locally (e.g., too large to store).
    /// - **`Invalid`**: Content failed validation (e.g. CBOR deserialization).
    ///
    /// **Idempotency**: The `Missing` state ensures content processing is
    /// idempotent. When `process_event_content_tx` is called, it checks for
    /// `Missing` state - if present, it processes the content and removes
    /// the marker. If absent (or Deleted/Pruned/Invalid), it skips
    /// processing. This prevents duplicate side effects when the same
    /// content arrives multiple times.
    events_content_state: ShortEventId => EventContentState
}

def_table! {
    /// Events whose content we want but haven't fetched yet, sorted by
    /// scheduled fetch time.
    ///
    /// Key: `(next_attempt_ts, event_id)` — sorted by next scheduled fetch
    /// time, so the fetcher can peek at the first entry and sleep until then.
    ///
    /// The `next_attempt_ts` component uses `Timestamp::ZERO` for newly
    /// inserted events (meaning "try immediately"). After each failed fetch
    /// attempt, the entry is re-inserted with an exponentially increasing
    /// timestamp based on the attempt count.
    ///
    /// The corresponding `EventContentState::Missing` entry stores a copy of
    /// `next_attempt_ts` (as `next_fetch_attempt`) to enable efficient
    /// removal from this table when the content state changes.
    ///
    /// **Note**: Events in this table already have their RC counted in
    /// `content_rc`. When content arrives, the event is removed from this
    /// table but RC stays the same.
    events_content_missing: (Timestamp, ShortEventId) => ()
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
    /// Key: (received_timestamp, reception_order)
    /// Value: event_id + reception source information
    ///
    /// The `reception_order` is a monotonically increasing counter that ensures
    /// strict ordering even when multiple events arrive at the same timestamp.
    /// The key `(Timestamp, u64)` is guaranteed unique - insertions assert this.
    ///
    /// This enables tracking network propagation delays (by comparing received
    /// timestamp vs event's author timestamp), debugging sync issues, and
    /// analytics about event acquisition patterns.
    events_received_at: (Timestamp, u64) => EventReceivedRecord
}

// ============================================================================
// SOCIAL TABLES
// Derived data extracted from social-related events for efficient querying.
// ============================================================================

def_table! {
    /// User profile information (display name, bio, avatar).
    ///
    /// Extracted from SOCIAL_PROFILE_UPDATE events. Only the latest profile
    /// per user is stored.
    social_profiles: RostraId => Latest<IdSocialProfileRecord>
}

def_table! {
    /// Post metadata (reply and reaction counts).
    ///
    /// This table stores aggregate counts for posts. The actual post content
    /// is in `events_content`. A record may exist here even before we receive
    /// the post itself (to track reply counts from replies we've seen).
    social_posts: (ShortEventId) => SocialPostRecord
}

def_table! {
    /// Index of replies to posts.
    ///
    /// Key: (parent_post_id, reply_timestamp, reply_event_id)
    /// Enables efficient retrieval of all replies to a post, ordered by time.
    social_posts_replies: (ShortEventId, Timestamp, ShortEventId) => SocialPostsRepliesRecord
}

def_table! {
    /// Index of reactions to posts.
    ///
    /// Key: (parent_post_id, reaction_timestamp, reaction_event_id)
    /// Enables efficient retrieval of all reactions to a post, ordered by time.
    social_posts_reactions: (ShortEventId, Timestamp, ShortEventId) => SocialPostsReactionsRecord
}

def_table! {
    /// Time-ordered index of social posts.
    ///
    /// Key: (timestamp, post_event_id)
    /// Used for timeline queries - fetching posts in chronological order.
    social_posts_by_time: (Timestamp, ShortEventId) => ()
}

def_table! {
    /// Time-ordered index of social posts by reception time.
    ///
    /// Key: (received_timestamp, reception_order)
    /// Value: post_event_id
    ///
    /// Used for notification queries - posts ordered by when we received them,
    /// not when they were authored. This is important for notifications where
    /// the order of reception matters more than the order of creation.
    ///
    /// The `reception_order` is a monotonically increasing counter that ensures
    /// strict ordering. The key `(Timestamp, u64)` is guaranteed unique.
    social_posts_by_received_at: (Timestamp, u64) => ShortEventId
}

def_table! {
    /// Posts that @mention the local user (self).
    ///
    /// Key: post_event_id
    ///
    /// This table records social posts that contain a `rostra:<self_id>` link,
    /// which represents an @mention of the local user. Used for notifications
    /// alongside reply detection.
    ///
    /// Only posts by other users are recorded here (self-mentions are not
    /// recorded since they are not notifications).
    social_posts_self_mention: ShortEventId => ()
}

// ============================================================================
// SHOUTBOX TABLES
// ============================================================================

def_table! {
    /// Time-ordered index of shoutbox posts by reception time.
    ///
    /// Key: (received_timestamp, reception_order)
    /// Value: post_event_id
    ///
    /// Similar to `social_posts_by_received_at` but for shoutbox posts.
    /// The `reception_order` is a monotonically increasing counter that ensures
    /// strict ordering even when multiple events arrive at the same timestamp.
    shoutbox_posts_by_received_at: (Timestamp, u64) => ShortEventId
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
