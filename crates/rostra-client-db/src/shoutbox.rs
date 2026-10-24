//! Bounded all-author shoutbox traversal, independent of social-post filtering.
//!
//! First-page capture and scan share a read transaction. Continuations exclude
//! allocation sequences at or above that exclusive bound, even when backdated.
//! Membership and payload availability are not frozen between pages. Cursors
//! expire on reopen or total rebuild; retain displayed rows and offer Refresh.

use rostra_core::event::content_kind::Shoutbox;
use rostra_core::id::RostraId;
use rostra_core::{EventId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::social::ReceivedAtPaginationCursor;
use crate::{Database, DbError, DbResult};

/// Maximum raw index entries consumed per call, including excluded allocations.
pub const SHOUTBOX_SCAN_MAX: usize = 4096;
/// Maximum encoded payload bytes read for decoding per call.
pub const SHOUTBOX_PAYLOAD_MAX: usize = 16 * 1024 * 1024;
/// Native presentation policy; larger historical content gets a placeholder.
pub const SHOUTBOX_CONTENT_MAX: usize = 1_000_000;

/// Validated independent output, raw-work and payload-work bounds.
#[derive(Clone, Copy, Debug)]
pub struct ShoutboxBounds {
    /// Output rows, including placeholders, in 1..=100.
    pub rows: usize,
    /// Raw consumed index rows in 1..=SHOUTBOX_SCAN_MAX.
    pub raw_rows: usize,
    /// Encoded bytes decoded in 1..=SHOUTBOX_PAYLOAD_MAX.
    pub payload_bytes: usize,
}

impl ShoutboxBounds {
    pub(crate) fn validate(self) -> DbResult<()> {
        if !(1..=100).contains(&self.rows)
            || !(1..=SHOUTBOX_SCAN_MAX).contains(&self.raw_rows)
            || !(1..=SHOUTBOX_PAYLOAD_MAX).contains(&self.payload_bytes)
        {
            return Err(DbError::ShoutboxBounds);
        }
        Ok(())
    }
}

/// Opaque local continuation, not authorization or a durable snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShoutboxCursor {
    /// Full account scope.
    pub(crate) account: RostraId,
    /// Durable rebuild incarnation shared with the reception allocator.
    pub(crate) epoch: [u8; 16],
    /// Per-open incarnation prevents reopen/restore allocation ABA.
    pub(crate) instance: [u8; 16],
    /// Exclusive allocation bound captured by the first page.
    pub(crate) next_sequence: u64,
    /// Last consumed raw row, whether or not emitted.
    pub(crate) position: ReceivedAtPaginationCursor,
}

/// Why available signed content cannot be presented through this bounded API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShoutboxIssue {
    /// Exceeds the whole caller byte budget or the native one-MB policy.
    Oversized,
    /// Outside the known flat-map decoder policy; not a protocol-invalid claim.
    Unsupported,
}

/// One exact signed identity and its received-order position.
#[derive(Clone, Debug)]
pub struct ShoutboxRow {
    /// Full author and event identity, bound to the retained signed envelope.
    pub target: (RostraId, EventId),
    /// Exclusive paging/deduplication position of this materialization.
    pub received: ReceivedAtPaginationCursor,
    /// Signed author timestamp, distinct from received display ordering.
    pub timestamp: Timestamp,
    /// Exact historical text when supported, never normalized during reads.
    pub content: Option<Shoutbox>,
    /// Explicit placeholder reason when content is absent.
    pub issue: Option<ShoutboxIssue>,
    /// Signed encoded payload length.
    pub encoded_bytes: u32,
}

/// Only End proves that the raw range is exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShoutboxStop {
    /// No further raw entries in this read transaction.
    End,
    /// Output row bound reached.
    PageLimit,
    /// Raw scan bound reached, possibly with no output rows.
    ScanBudget,
    /// Next payload fits a fresh budget but not its remainder; unconsumed.
    PayloadBudget,
}

/// One bounded page; empty non-End pages still require continuation.
#[derive(Clone, Debug)]
pub struct ShoutboxPage {
    /// All-author rows in descending received order, including placeholders.
    pub rows: Vec<ShoutboxRow>,
    /// More raw scanning, not a promise of another available row.
    pub next_cursor: Option<ShoutboxCursor>,
    /// Exact reason this scan stopped.
    pub stop: ShoutboxStop,
    /// Consumed raw entries; one additional boundary probe may occur.
    pub raw_rows_scanned: usize,
    /// Encoded bytes passed to the bounded decoder.
    pub payload_bytes_read: usize,
    /// Consumed rows whose content is missing, deleted or pruned.
    pub unavailable_rows: usize,
}

impl Database {
    /// Capture and scan, or continue with the original exclusive allocation
    /// bound.
    ///
    /// No total traversal cap applies. Storage/corruption errors return no
    /// partial page. Work bounds are not a CPU deadline or process RSS
    /// guarantee.
    pub async fn paginate_shoutbox(
        &self,
        cursor: Option<ShoutboxCursor>,
        bounds: ShoutboxBounds,
    ) -> DbResult<ShoutboxPage> {
        bounds.validate()?;
        self.read_with(|tx| self.scan_shoutbox_tx(tx, cursor.as_ref(), bounds))
            .await
    }
}
