//! Exact, bounded snapshot counts and independently paginated retained rows.

use rostra_core::id::RostraId;
use rostra_core::{EventId, Timestamp};

/// Exclusive descending timestamp/full-event-ID cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReactionCursor {
    /// Signed timestamp used for row ordering.
    pub timestamp: Timestamp,
    /// Full retained event identity, breaking timestamp ties.
    pub event_id: EventId,
}

/// One available supported materialized reaction; not a unique-author vote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionRow {
    /// Full retained author identity.
    pub author: RostraId,
    /// Full retained reaction identity and timestamp.
    pub cursor: ReactionCursor,
    /// Exact legacy-compatible reaction string.
    pub reaction: String,
}

/// A single read snapshot; limit failures return no partial summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionSummary {
    /// Complete supported materialized row count across canonical target
    /// history.
    pub total: u64,
    /// Complete count authored by this database's identity, not a boolean.
    pub self_count: u64,
    /// Requested descending page, independent from the complete counts.
    pub rows: Vec<ReactionRow>,
    /// Exclusive continuation only when further eligible rows exist.
    pub next_cursor: Option<ReactionCursor>,
    /// Raw index entries inspected, including unavailable/unsupported rows.
    pub raw_rows_scanned: usize,
}
