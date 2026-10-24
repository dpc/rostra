//! Bounded discovery of an author's locally observed post labels.
//!
//! This uses the same materialized time index as `get_persona_tags_for_id`:
//! replies, reactions, news and retained replaced versions can contribute.
//! Deletion and quota dematerialization remove ordinary index rows;
//! unavailable/pruned payloads contribute no labels and are reported only when
//! an index row remains. Removed and never-materialized events are outside the
//! scan and its omission counters. There is no history traversal,
//! identity-secret access, networking, profile lookup or tag catalog. An empty
//! result means no labels observed in this window, not an unknown identity.
//!
//! Each call reads one live snapshot, not a captured cross-page snapshot. New
//! rows ahead of the descending cursor are missed; removed rows and labels
//! already merged by the caller can become stale. Refresh from the beginning to
//! reconcile. Cursors expire on reopen or projection rebuild.
//!
//! Deduplication is page-local. Callers merge pages and accumulate skipped
//! counts. Even `End` with no skips means only exhaustion of this call's local
//! index, never all tags used by an identity across the network.

use std::collections::BTreeSet;

use rostra_core::event::PersonaTag;
use rostra_core::event::content_kind::discovered_tags::{ITEMS_MAX, PAYLOAD_BYTES_MAX};
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{Database, DbError, DbResult};

/// Maximum raw global index rows examined per page, including unrelated
/// authors.
pub const AUTHOR_TAG_ROWS_MAX: usize = 500;
/// Maximum unique output tags per page (at most 1024 UTF-8 tag bytes).
pub const AUTHOR_TAG_OUTPUT_MAX: usize = 32;
/// Per-row conservative byte reservation: payload plus envelope/keys/state.
pub const AUTHOR_TAG_BYTES_MIN: usize = PAYLOAD_BYTES_MAX + 2048;
/// Maximum per-call encoded byte allowance.
pub const AUTHOR_TAG_BYTES_MAX: usize = 4 * 1024 * 1024;
/// Per-row work reservation, covering CBOR items and bounded decoding.
pub const AUTHOR_TAG_WORK_MIN: usize = ITEMS_MAX;
/// Maximum per-call CBOR item reservation.
pub const AUTHOR_TAG_WORK_MAX: usize = ITEMS_MAX * 64;

/// Independent per-call limits, validated before opening a read transaction.
#[derive(Clone, Copy, Debug)]
pub struct AuthorTagBounds {
    /// Raw index entries, in 1..=AUTHOR_TAG_ROWS_MAX.
    pub raw_rows: usize,
    /// Reserved encoded bytes, in AUTHOR_TAG_BYTES_MIN..=AUTHOR_TAG_BYTES_MAX.
    pub bytes: usize,
    /// Reserved CBOR item capacity, in
    /// AUTHOR_TAG_WORK_MIN..=AUTHOR_TAG_WORK_MAX.
    pub work: usize,
    /// Unique returned tags, in 1..=AUTHOR_TAG_OUTPUT_MAX.
    pub tags: usize,
}

impl Default for AuthorTagBounds {
    fn default() -> Self {
        Self {
            raw_rows: 64,
            bytes: AUTHOR_TAG_BYTES_MAX,
            work: AUTHOR_TAG_WORK_MAX,
            tags: AUTHOR_TAG_OUTPUT_MAX,
        }
    }
}

impl AuthorTagBounds {
    /// Validate all limits; minima guarantee progress on an existing row.
    pub fn validate(self) -> DbResult<()> {
        if !(1..=AUTHOR_TAG_ROWS_MAX).contains(&self.raw_rows)
            || !(AUTHOR_TAG_BYTES_MIN..=AUTHOR_TAG_BYTES_MAX).contains(&self.bytes)
            || !(AUTHOR_TAG_WORK_MIN..=AUTHOR_TAG_WORK_MAX).contains(&self.work)
            || !(1..=AUTHOR_TAG_OUTPUT_MAX).contains(&self.tags)
        {
            return Err(DbError::AuthorTagBounds);
        }
        Ok(())
    }
}

/// Opaque local continuation, serializable but not authorization.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorTagCursor {
    /// Database owner, independent of the queried author.
    pub(crate) account: RostraId,
    /// Exact queried author.
    pub(crate) author: RostraId,
    /// Persistent projection incarnation.
    pub(crate) epoch: [u8; 16],
    /// Per-open incarnation.
    pub(crate) instance: [u8; 16],
    /// Descending time-index position.
    pub(crate) position: (Timestamp, ShortEventId),
    /// Next sorted effective tag within this immutable payload; zero excludes
    /// the completed row. Positive values include and revalidate that row.
    pub(crate) offset: usize,
}

/// Why the bounded call stopped; budget stops may precede an empty final page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorTagStop {
    /// Observed exhaustion of the local index in this read snapshot.
    End,
    /// Raw row allowance exhausted.
    Rows,
    /// Insufficient remaining byte reservation for another candidate.
    Bytes,
    /// Insufficient remaining item reservation for another candidate.
    Work,
    /// Output limit reached, possibly partway through one payload.
    Tags,
}

/// Page-local omissions; accumulate these across the caller's traversal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorTagSkipped {
    /// Missing retained envelope, or author's missing/unavailable content.
    pub unavailable: usize,
    /// Payload record exceeds the discovery byte cap.
    pub oversized: usize,
    /// Unsupported/malformed payload, excessive CBOR work, or invalid tag.
    pub unsupported: usize,
}

/// Bounded labels and honest continuation metadata from one live read.
#[derive(Clone, Debug)]
pub struct AuthorTagPage {
    /// Sorted, page-local unique labels; not a complete or authoritative
    /// catalog.
    pub tags: BTreeSet<PersonaTag>,
    /// Present on budget/output stops, absent only on observed exhaustion.
    pub next_cursor: Option<AuthorTagCursor>,
    /// Local exhaustion versus a conservative continuation.
    pub stop: AuthorTagStop,
    /// Raw global entries consumed, including skipped and unrelated rows.
    pub raw_rows_scanned: usize,
    /// Conservative reservations, not physical disk I/O or heap allocation.
    pub bytes_reserved: usize,
    /// Conservative CBOR item capacity reserved, not elapsed CPU time.
    pub work_reserved: usize,
    /// Rows that could not supply tags. Nonzero means discovery is incomplete
    /// even when the local index is exhausted.
    pub skipped: AuthorTagSkipped,
}

impl Database {
    /// Discover bounded locally observed labels for self or another author.
    ///
    /// Raw checked reads avoid typed redb iterator decode panics. Corrupt
    /// in-budget storage records fail the whole call; bounded unsupported CBOR
    /// produces an explicit omission, never a fabricated empty tag set.
    /// Output resumes within a payload after rechecking its exact event,
    /// timestamp, author, content hash and current availability. A removed row
    /// advances to the next key. Page-local dedup does not prevent re-emission
    /// from later posts or later pages. Run off the UI executor with bounded
    /// concurrency: B-tree seeks still depend on database size and these are
    /// not RSS or latency guarantees.
    pub async fn discover_author_tags_page(
        &self,
        author: RostraId,
        cursor: Option<&AuthorTagCursor>,
        bounds: AuthorTagBounds,
    ) -> DbResult<AuthorTagPage> {
        bounds.validate()?;
        self.read_with(|tx| self.author_tags_page_tx(tx, author, cursor, bounds))
            .await
    }
}
