//! Identity-related database record types.
//!
//! These types store information about user identities and their relationships
//! (follows, followers, personas).

use bincode::{Decode, Encode};
use rostra_core::Timestamp;
use rostra_core::event::{PersonaSelector, PersonaTag, PersonasTagsSelector};

/// Record for the `ids_followees` table.
///
/// Stored with key `(follower_id, followee_id)`, this tracks who someone
/// follows and which of their personas they want to see.
///
/// Entries in the table represent active follows. When an unfollow event
/// is processed, the entry is removed from the table entirely (and recorded
/// in `ids_unfollowed` instead).
#[derive(Debug, Encode, Decode, Clone)]
pub struct IdsFolloweesRecord {
    /// Timestamp of the latest follow/unfollow event (used as idempotency
    /// guard)
    pub latest_ts: Timestamp,
    /// Timestamp of the first follow event that established this relationship.
    ///
    /// Used for notification timestamp heuristics: posts from before this
    /// time are likely historical syncs and should not appear as "just
    /// received".
    pub first_ts: Timestamp,
    /// Legacy persona selector — kept for backward compat with old follow
    /// events.
    selector: Option<PersonaSelector>,
    /// New tag-based selector.
    ///
    /// When present, this takes priority over the legacy `selector`.
    tags_selector: Option<PersonasTagsSelector>,
}

impl IdsFolloweesRecord {
    /// Create a new followee record.
    pub fn new(
        latest_ts: Timestamp,
        first_ts: Timestamp,
        selector: Option<PersonaSelector>,
        tags_selector: Option<PersonasTagsSelector>,
    ) -> Self {
        Self {
            latest_ts,
            first_ts,
            selector,
            tags_selector,
        }
    }

    /// Returns the effective tag-based selector for this follow.
    ///
    /// Returns `tags_selector` when set. For legacy records that only
    /// have the old id-based `selector`, converts the legacy
    /// `PersonaId`s to `PersonaTag`s (0=personal, 1=professional,
    /// 2=civic). Unknown ids are dropped during conversion.
    pub fn effective_tags_selector(&self) -> PersonasTagsSelector {
        if let Some(ref ts) = self.tags_selector {
            return ts.clone();
        }
        if let Some(ref s) = self.selector {
            return match s {
                PersonaSelector::Only { ids } => PersonasTagsSelector::Only {
                    ids: ids
                        .iter()
                        .filter_map(|id| PersonaTag::from_persona_id(*id))
                        .collect(),
                },
                PersonaSelector::Except { ids } => PersonasTagsSelector::Except {
                    ids: ids
                        .iter()
                        .filter_map(|id| PersonaTag::from_persona_id(*id))
                        .collect(),
                },
            };
        }
        PersonasTagsSelector::default()
    }
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
