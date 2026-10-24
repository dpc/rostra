//! Bounded reads of the owner's current direct relationships.
//!
//! Each call reads one database snapshot, including outgoing follow-back
//! selectors on follower rows. Pages are **not** a cross-call snapshot:
//! additions behind the exclusive identity cursor are missed, removed displayed
//! rows remain stale, and changes ahead appear on subsequent calls. Mutations
//! do not invalidate cursors automatically. Refresh from the beginning to
//! reconcile; do not present a traversal under mutation as a complete
//! consistent set.
//!
//! Cursors expire on reopen or projection rebuild and cannot cross accounts or
//! directions. They are local continuation hints, not authorization. These APIs
//! do not change retained state, ingestion, watch projections or wire formats.
//!
//! No profile bodies, avatars, names, sorting by name, or transitive follows
//! are read. There is no hidden filtering scan: every active index entry emits
//! a row, even when its selector exceeds the bounded presentation policy. `Only
//! {}` is an active follow whose filter matches no posts, never an unfollow.

use rostra_core::event::PersonasTagsSelector;
use rostra_core::id::RostraId;
use serde::{Deserialize, Serialize};

use crate::{Database, DbError, DbResult};

/// Maximum active index rows read and returned per page.
pub const RELATIONSHIP_ROWS_MAX: usize = 100;
/// Maximum encoded follow record admitted to selector validation and decoding.
pub const RELATIONSHIP_RECORD_BYTES_MAX: usize = 16 * 1024;
/// Maximum encoded selector elements, summed across legacy and tag selectors.
pub const RELATIONSHIP_SELECTOR_ELEMENTS_MAX: usize = 256;
/// Minimum page allowance: one maximum record and both relationship keys.
pub const RELATIONSHIP_BYTES_MIN: usize = RELATIONSHIP_RECORD_BYTES_MAX + 128;
/// Maximum encoded validation/decode allowance per page.
pub const RELATIONSHIP_BYTES_MAX: usize = 2 * 1024 * 1024;

/// Which direct active relationship index to traverse for the database owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipDirection {
    /// Identities the owner follows.
    Following,
    /// Identities following the owner.
    Followers,
}

/// Independent output and encoded validation/decode limits.
#[derive(Clone, Copy, Debug)]
pub struct RelationshipBounds {
    /// Active index rows, in 1..=RELATIONSHIP_ROWS_MAX.
    pub rows: usize,
    /// Bytes, in RELATIONSHIP_BYTES_MIN..=RELATIONSHIP_BYTES_MAX.
    ///
    /// Before reading each row we conservatively reserve a maximum-sized record
    /// and two keys. Unused capacity can therefore cause an early continuation.
    /// The minimum guarantees at least one row of progress when one exists.
    pub bytes: usize,
}

impl RelationshipBounds {
    pub(crate) fn validate(self) -> DbResult<()> {
        if !(1..=RELATIONSHIP_ROWS_MAX).contains(&self.rows)
            || !(RELATIONSHIP_BYTES_MIN..=RELATIONSHIP_BYTES_MAX).contains(&self.bytes)
        {
            return Err(DbError::RelationshipBounds);
        }
        Ok(())
    }
}

/// Opaque, serializable exclusive identity continuation for one open database.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipCursor {
    /// Exact local owner.
    pub(crate) account: RostraId,
    /// Index whose identity order this cursor traverses.
    pub(crate) direction: RelationshipDirection,
    /// Persistent projection rebuild incarnation.
    pub(crate) epoch: [u8; 16],
    /// Per-open incarnation; persisted cursors require refresh after restart.
    pub(crate) instance: [u8; 16],
    /// Last emitted identity, excluded by the next raw range seek.
    pub(crate) position: RostraId,
}

/// Why a confirmed relationship's selector cannot be exposed by this API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationshipIssue {
    /// Encoded record bytes or encoded selector elements exceed local limits.
    ///
    /// Over-budget records are not fully validated; this is not a claim that
    /// their encoding is valid or invalid.
    Oversized,
}

/// Exact effective selector, or explicit inability to read it within the
/// limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelationshipSelector {
    /// The authoritative effective selector, including legacy conversion.
    Available(PersonasTagsSelector),
    /// Relationship membership is known, but its selector is not available.
    Unavailable(RelationshipIssue),
}

/// The owner's exact outgoing relationship with one full identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelationshipState {
    /// No active outgoing key exists in this read snapshot.
    NotFollowing,
    /// An active outgoing key exists, including an empty `Only` filter.
    Following {
        /// Effective selector or an explicit bounded-read limitation.
        selector: RelationshipSelector,
    },
}

/// One active member of the selected index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationshipRow {
    /// Full identity for exact navigation and subsequent relationship
    /// operations.
    pub identity: RostraId,
    /// OUR outgoing follow, never the follower's incoming selector.
    ///
    /// Always `Following` on following pages. On follower pages this is a
    /// bounded exact point read from the same transaction as membership.
    pub own_follow: RelationshipState,
}

/// Why this bounded call stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationshipStop {
    /// The range was exhausted in this call's read snapshot.
    End,
    /// Row allowance reached; another call may return an empty terminal page.
    PageLimit,
    /// Insufficient remaining capacity to reserve another maximum-sized row.
    ByteBudget,
}

/// Bounded identity-ordered rows from one current read snapshot.
#[derive(Clone, Debug)]
pub struct RelationshipPage {
    /// Ascending full-identity order, including unavailable-selector rows.
    pub rows: Vec<RelationshipRow>,
    /// Present for budget stops, absent only for observed snapshot exhaustion.
    pub next_cursor: Option<RelationshipCursor>,
    /// Exhaustion versus a conservative continuation.
    pub stop: RelationshipStop,
    /// Active index rows consumed; at most `bounds.rows`.
    pub raw_rows_scanned: usize,
    /// Exact outgoing lookups on follower pages; at most `bounds.rows`.
    pub point_reads: usize,
    /// Encoded key and in-budget record bytes charged to validation/decoding.
    ///
    /// Includes known outgoing point keys. Oversized records are length-checked
    /// without copying or decoding their bytes and are not charged here.
    /// This is not physical disk I/O, decoded heap size, RSS or a CPU deadline.
    /// Selector element and record-byte caps independently bound owned output.
    pub bytes_charged: usize,
}

impl Database {
    /// Read a bounded live page of the owner's following or followers.
    ///
    /// Uses raw checked range iteration, not a clone of a whole watch snapshot.
    /// All in-budget key/record decode failures and storage errors fail the
    /// whole call, never a partial page or fabricated empty membership.
    /// Oversized selectors produce explicit placeholders preserving active
    /// membership. B-tree seek costs still depend on database size. Run off
    /// a UI executor with bounded concurrency; these limits are not
    /// latency/RSS guarantees.
    pub async fn get_self_relationships_page(
        &self,
        direction: RelationshipDirection,
        cursor: Option<&RelationshipCursor>,
        bounds: RelationshipBounds,
    ) -> DbResult<RelationshipPage> {
        bounds.validate()?;
        self.read_with(|tx| self.relationship_page_tx(tx, direction, cursor, bounds))
            .await
    }

    /// Read the owner's exact outgoing relationship without enumerating
    /// follows.
    ///
    /// One raw point lookup, at most RELATIONSHIP_RECORD_BYTES_MAX encoded
    /// record bytes and RELATIONSHIP_SELECTOR_ELEMENTS_MAX selector
    /// elements. Missing means `NotFollowing`; oversized means confirmed
    /// `Following` with an unavailable selector; malformed in-budget data
    /// or storage failure is an error. This says nothing about whether the
    /// identity has a known profile.
    pub async fn get_self_relationship(&self, identity: RostraId) -> DbResult<RelationshipState> {
        self.read_with(|tx| {
            let followees = tx.open_table(&crate::ids_followees::TABLE)?;
            let (state, _) =
                crate::relationship_read::own_follow(&followees, self.self_id, identity)?;
            Ok(state)
        })
        .await
    }
}
