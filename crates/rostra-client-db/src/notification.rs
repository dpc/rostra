//! Bounded notification reads, with host-owned seen-through-refresh state.
//!
//! Selection matches the web notification predicate: exclude own posts, then
//! include replies to self or indexed self-mentions. Display order remains
//! descending effective `(received timestamp, sequence)`. This timestamp can
//! move backwards, both deliberately for historical sync and after clock
//! rollback. Unread therefore uses the durable allocation frontier instead of
//! the web's tuple cutoff. Payload materialization (including restoration and
//! edits) allocates a fresh sequence and indexes mentions in the same
//! transaction. Ordinary redelivery does not restore pruned/deleted/invalid
//! payloads. Total replay rebuilds receipt and mention indexes together and
//! changes the epoch.
//!
//! No method writes seen state. A host may acknowledge only a first-page
//! `highwater`, after successfully publishing that page, even when it is empty.
//! Pagination never supplies a new acknowledgment. This means seen through a
//! refresh, not individually read: it acknowledges even rows beyond that page.
//!
//! Tokens survive ordinary reopen, but total replay/new databases change their
//! epoch. A restored backup older than a token's frontier is rejected. Faithful
//! copies share an epoch; divergent copies or a rollback followed by enough new
//! writes cannot be detected. Hosts must discard tokens when replacing storage
//! from such a backup. Tokens are trusted local state, not authorization or
//! authenticated remote input.

use rostra_core::EventId;
use rostra_core::event::SocialPost;
use rostra_core::id::RostraId;
use serde::{Deserialize, Serialize};

use crate::social::ReceivedAtPaginationCursor;
use crate::{Database, DbError, DbResult};

/// Hard upper bound on raw index entries consumed per call.
pub const NOTIFICATION_SCAN_MAX: usize = 4_096;
/// Hard upper bound on encoded payload bytes decoded per call.
pub const NOTIFICATION_PAYLOAD_MAX: usize = 16 * 1024 * 1024;

/// Caller-selected work bounds, validated before opening the read transaction.
#[derive(Clone, Copy, Debug)]
pub struct NotificationBounds {
    /// Maximum confirmed notification rows, in `1..=100`.
    pub rows: usize,
    /// Maximum raw rows, in `1..=NOTIFICATION_SCAN_MAX`.
    pub raw_rows: usize,
    /// Maximum encoded bytes decoded, in `1..=NOTIFICATION_PAYLOAD_MAX`.
    pub payload_bytes: usize,
}

impl NotificationBounds {
    pub(crate) fn validate(self) -> DbResult<()> {
        if !(1..=100).contains(&self.rows)
            || !(1..=NOTIFICATION_SCAN_MAX).contains(&self.raw_rows)
            || !(1..=NOTIFICATION_PAYLOAD_MAX).contains(&self.payload_bytes)
        {
            return Err(DbError::NotificationBounds);
        }
        Ok(())
    }
}

/// Opaque durable seen-through-refresh boundary for one account/database epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationSeen {
    /// Owning full identity.
    pub(crate) account: RostraId,
    /// Durable receipt-index incarnation.
    pub(crate) epoch: [u8; 16],
    /// Next unused allocation sequence, not a display-order cutoff.
    pub(crate) next_sequence: u64,
    /// Latest global social receipt, including nonnotifications and own posts.
    pub(crate) received: Option<ReceivedAtPaginationCursor>,
}

impl NotificationSeen {
    /// Latest global social receipt in the capturing transaction, possibly
    /// absent.
    pub fn received_highwater(&self) -> Option<ReceivedAtPaginationCursor> {
        self.received
    }
}

/// Exclusive descending continuation, retaining the first page's arrival bound.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationCursor {
    /// Initial transaction's allocation frontier and scope.
    pub(crate) snapshot: NotificationSeen,
    /// Last consumed raw row, not last emitted notification.
    pub(crate) position: ReceivedAtPaginationCursor,
}

/// Independently observable notification reasons; both may apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationReason {
    /// Whether the decoded reply target is self; unknown for oversized content.
    pub reply: Option<bool>,
    /// Membership in the current self-mention index.
    pub mention: bool,
}

/// Confirmed notification with exact signed navigation identity.
#[derive(Clone, Debug)]
pub struct NotificationRow {
    /// Full retained author and event identity.
    pub target: (RostraId, EventId),
    /// Display-order position of this materialization.
    pub received: ReceivedAtPaginationCursor,
    /// Validated content, or absent for an oversized confirmed mention.
    pub content: Option<SocialPost>,
    /// Signed encoded length, useful for unavailable-body presentation.
    pub encoded_bytes: u32,
    /// Reply and indexed-mention classification.
    pub reason: NotificationReason,
}

/// Unclassified candidate whose reply membership cannot be determined in
/// budget.
///
/// This is not a confirmed notification or an invalid protocol payload. It has
/// no indexed mention, but its content exceeds this call's entire byte budget.
/// Hosts must disclose incomplete classification rather than hide these
/// entries.
#[derive(Clone, Debug)]
pub struct NotificationIssue {
    /// Full retained author and event identity.
    pub target: (RostraId, EventId),
    /// Consumed raw display position.
    pub received: ReceivedAtPaginationCursor,
    /// Signed encoded length exceeding the caller's budget.
    pub encoded_bytes: u32,
}

/// Why scanning stopped; only End proves raw-index exhaustion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationStop {
    /// No eligible range entries remain in this call's transaction.
    End,
    /// Requested confirmed-row count reached.
    PageLimit,
    /// Raw work budget reached; continue even when no notifications were found.
    ScanBudget,
    /// Next payload fits the whole budget but not its remainder; not consumed.
    PayloadBudget,
}

/// One bounded received-order page, never an implicit acknowledgment.
#[derive(Clone, Debug)]
pub struct NotificationPage {
    /// Confirmed notifications in descending received order.
    pub rows: Vec<NotificationRow>,
    /// Exclusive progress over raw rows; absent only at End.
    pub next_cursor: Option<NotificationCursor>,
    /// Global boundary captured with the first page; absent for continuation.
    pub highwater: Option<NotificationSeen>,
    /// Explicit termination reason.
    pub stop: NotificationStop,
    /// Consumed raw rows, including filtered and oversized candidates.
    pub raw_rows_scanned: usize,
    /// Encoded payload bytes decoded, including nonmatching posts.
    pub payload_bytes_read: usize,
    /// Oversized nonmention candidates, not classified as notifications.
    pub issues: Vec<NotificationIssue>,
}

/// Exact witnessed presence, proven absence, or unfinished/unclassifiable work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationUnread {
    /// A currently visible qualifying arrival after seen was witnessed.
    Yes,
    /// The scan exhausted with no qualifying or unclassified arrival.
    No,
    /// Continue scanning, or disclose terminal oversized uncertainty.
    Unknown,
}

/// Opaque unread continuation, bound to both seen and the scan's initial
/// frontier.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationUnreadCursor {
    /// Original host-owned seen token.
    pub(crate) seen: Option<NotificationSeen>,
    /// Shared received traversal state.
    pub(crate) scan: NotificationCursor,
    /// An oversized candidate was encountered on an earlier page.
    pub(crate) unresolved: bool,
}

/// Bounded unread evidence; snapshot is descriptive, never mark-seen authority.
#[derive(Clone, Debug)]
pub struct NotificationUnreadPage {
    /// Unknown must not be rendered or persisted as false.
    pub state: NotificationUnread,
    /// More scan work; Unknown without a cursor is terminal size uncertainty.
    pub next_cursor: Option<NotificationUnreadCursor>,
    /// Observed boundary for this query, not a visible first-page
    /// acknowledgment.
    pub snapshot: NotificationSeen,
    /// Explicit raw scan termination.
    pub stop: NotificationStop,
    /// Raw rows consumed in this call.
    pub raw_rows_scanned: usize,
    /// Encoded payload bytes decoded in this call.
    pub payload_bytes_read: usize,
    /// Unclassified candidates encountered in this call.
    pub issues: Vec<NotificationIssue>,
}

impl Database {
    /// Read a bounded page with web notification membership and received order.
    ///
    /// A first page captures the latest *global* receipt and allocation
    /// frontier in the same transaction, including for an empty result.
    /// Continuations exclude new allocations even if backdated. Current
    /// deletion/replacement can remove rows between pages; this is not a
    /// retained database snapshot.
    ///
    /// Work is bounded by raw entries, encoded payload bytes, and constant-size
    /// metadata lookups per entry; at most one unconsumed boundary probe is
    /// read. No replacement lineage or entire mention index is loaded. Byte
    /// bounds are not a process RSS or CPU deadline; decoded values can add
    /// overhead. Oversized confirmed mentions have no content; other
    /// oversized candidates appear as issues. Neither is silently discarded
    /// or called invalid.
    ///
    /// Invalid bounds, stale scope/future frontiers, storage and invariant
    /// errors return no page or acknowledgment. Reopen preserves tokens;
    /// total replay invalidates them. On stale seen state, hosts must reset
    /// conservatively, never silently acknowledge a new frontier.
    pub async fn paginate_notifications(
        &self,
        cursor: Option<NotificationCursor>,
        bounds: NotificationBounds,
    ) -> DbResult<NotificationPage> {
        bounds.validate()?;
        self.read_with(|tx| {
            let current = self.notification_snapshot_tx(tx)?;
            let first = cursor.is_none();
            let snapshot = cursor.as_ref().map_or(&current, |c| &c.snapshot);
            Self::validate_notification_token(snapshot, &current)?;
            let mut page = self.scan_notifications_tx(
                tx,
                snapshot,
                cursor.as_ref().map(|c| c.position),
                0,
                bounds,
            )?;
            page.highwater = first.then_some(current);
            Ok(page)
        })
        .await
    }

    /// Look for a visible notification allocated after host-owned `seen`.
    ///
    /// This may scan the entire received index because timestamp order is not
    /// allocation order. Every call is bounded; sparse pages return Unknown
    /// with progress. Unclassified oversized candidates preserve Unknown
    /// even at End, with no continuation: disclose that limitation, do not
    /// busy-retry. Indexed oversized mentions are sufficient for Yes.
    /// To reconsider earlier oversized candidates with larger bounds, start a
    /// new scan rather than resuming a cursor that remembers uncertainty.
    ///
    /// Continuations require the same seen token and unchanged allocation
    /// frontier; concurrent writes return NotificationCursorStale, not false.
    /// Callers should restart an invalidated query and retain Unknown
    /// meanwhile. No is as of the reported snapshot, never a guarantee
    /// against later writes. Reads never update any acknowledgment,
    /// including on Yes or No.
    pub async fn scan_notifications_unread(
        &self,
        seen: Option<NotificationSeen>,
        cursor: Option<NotificationUnreadCursor>,
        bounds: NotificationBounds,
    ) -> DbResult<NotificationUnreadPage> {
        bounds.validate()?;
        self.read_with(|tx| {
            let current = self.notification_snapshot_tx(tx)?;
            if let Some(seen) = &seen {
                Self::validate_notification_token(seen, &current)?;
            }
            if let Some(cursor) = &cursor {
                if cursor.seen != seen {
                    return Err(DbError::NotificationCursorMismatch);
                }
                Self::validate_notification_token(&cursor.scan.snapshot, &current)?;
                if cursor.scan.snapshot.next_sequence != current.next_sequence {
                    return Err(DbError::NotificationCursorStale);
                }
            }
            let snapshot = cursor
                .as_ref()
                .map_or_else(|| current.clone(), |c| c.scan.snapshot.clone());
            let page = self.scan_notifications_tx(
                tx,
                &snapshot,
                cursor.as_ref().map(|c| c.scan.position),
                seen.as_ref().map_or(0, |s| s.next_sequence),
                NotificationBounds { rows: 1, ..bounds },
            )?;
            let unresolved =
                cursor.as_ref().is_some_and(|c| c.unresolved) || !page.issues.is_empty();
            let state = if !page.rows.is_empty() {
                NotificationUnread::Yes
            } else if page.stop == NotificationStop::End && !unresolved {
                NotificationUnread::No
            } else {
                NotificationUnread::Unknown
            };
            Ok(NotificationUnreadPage {
                state,
                next_cursor: if state == NotificationUnread::Yes {
                    None
                } else {
                    page.next_cursor.map(|scan| NotificationUnreadCursor {
                        seen,
                        scan,
                        unresolved,
                    })
                },
                snapshot,
                stop: page.stop,
                raw_rows_scanned: page.raw_rows_scanned,
                payload_bytes_read: page.payload_bytes_read,
                issues: page.issues,
            })
        })
        .await
    }
}
