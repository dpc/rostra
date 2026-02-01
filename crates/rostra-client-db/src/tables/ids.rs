//! Identity-related database record types.
//!
//! These types store information about user identities and their relationships
//! (follows, followers, personas).

use bincode::{Decode, Encode};
use rostra_core::Timestamp;
use rostra_core::event::{PersonaId, PersonaSelector};
use rostra_core::id::RestRostraId;

/// Record for reconstructing full RostraId from ShortRostraId.
///
/// Used by the `ids_full` table to map shortened IDs back to full IDs.
#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdRecord {
    /// The "rest" of the RostraId (the part not in ShortRostraId)
    pub id_rest: RestRostraId,
}

/// Legacy followees record format (V0).
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFolloweesRecordV0 {
    pub ts: Timestamp,
    pub persona: PersonaId,
}

/// Record for the `ids_followees` table.
///
/// Stored with key `(follower_id, followee_id)`, this tracks who someone
/// follows and which of their personas they want to see.
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFolloweesRecord {
    /// Timestamp of the follow/unfollow event that established this state
    pub ts: Timestamp,
    /// Which personas from the followee to show.
    ///
    /// - `Some(selector)`: Active follow with the given persona filter
    /// - `None`: Pending unfollow - entry kept to track timestamp but follow is
    ///   inactive
    pub selector: Option<PersonaSelector>,
}

/// Record for the `ids_followers` table.
///
/// Stored with key `(followee_id, follower_id)`. This is a reverse index of
/// `ids_followees` for efficient "who follows me?" queries. Currently empty
/// as the key contains all needed information.
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFollowersRecord {}

/// Record for the `ids_unfollowed` table.
///
/// Tracks when an unfollow happened to prevent reprocessing old follow events
/// that have timestamps before the unfollow.
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsUnfollowedRecord {
    /// Timestamp when the unfollow occurred
    pub ts: Timestamp,
}

/// Record for the `ids_personas` table.
///
/// Users can define custom personas to categorize their posts (beyond the
/// default Personal/Professional/Civic personas).
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsPersonaRecord {
    /// Timestamp when this persona was created/updated
    pub ts: u64,
    /// Display name for this persona
    pub display_name: String,
}
