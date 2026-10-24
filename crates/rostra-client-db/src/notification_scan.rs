//! Shared bounded received-index reader. It deliberately avoids thread
//! metadata.

use std::ops::Bound;

use redb_bincode::ReadTransaction;
use rostra_core::event::{EventExt as _, EventKind, SocialPost};
use rostra_core::id::ToShort as _;

use crate::notification::{
    NotificationBounds, NotificationCursor, NotificationIssue, NotificationPage,
    NotificationReason, NotificationRow, NotificationSeen, NotificationStop,
};
use crate::social::ReceivedAtPaginationCursor;
use crate::{Database, DbError, DbResult};

impl Database {
    pub(crate) fn notification_snapshot_tx(
        &self,
        tx: &ReadTransaction,
    ) -> DbResult<NotificationSeen> {
        let epoch = tx
            .open_table(&crate::social_notification_epoch::TABLE)?
            .get(&())?
            .ok_or(DbError::NotificationInvariant)?
            .value_try()?;
        let next_sequence = tx
            .open_table(&crate::reception_order_next::TABLE)?
            .get(&())?
            .map(|row| row.value_try())
            .transpose()?
            .unwrap_or(0);
        let received = tx
            .open_table(&crate::social_posts_by_received_at::TABLE)?
            .last()?
            .map(|(key, _)| key.value_try())
            .transpose()?
            .map(|(ts, seq)| ReceivedAtPaginationCursor { ts, seq });
        Ok(NotificationSeen {
            account: self.self_id,
            epoch,
            next_sequence,
            received,
        })
    }

    pub(crate) fn validate_notification_token(
        token: &NotificationSeen,
        current: &NotificationSeen,
    ) -> DbResult<()> {
        if token.account != current.account
            || token.epoch != current.epoch
            || token.next_sequence > current.next_sequence
        {
            return Err(DbError::NotificationCursorStale);
        }
        Ok(())
    }

    pub(crate) fn scan_notifications_tx(
        &self,
        tx: &ReadTransaction,
        snapshot: &NotificationSeen,
        after: Option<ReceivedAtPaginationCursor>,
        minimum_sequence: u64,
        bounds: NotificationBounds,
    ) -> DbResult<NotificationPage> {
        let mut page = NotificationPage {
            rows: Vec::new(),
            next_cursor: None,
            highwater: None,
            stop: NotificationStop::End,
            raw_rows_scanned: 0,
            payload_bytes_read: 0,
            issues: Vec::new(),
        };
        if minimum_sequence == snapshot.next_sequence {
            return Ok(page);
        }
        let receipts = tx.open_table(&crate::social_posts_by_received_at::TABLE)?;
        let events = tx.open_table(&crate::events::TABLE)?;
        let states = tx.open_table(&crate::events_content_state::TABLE)?;
        let store = tx.open_table(&crate::content_store::TABLE)?;
        let replacements = tx.open_table(&crate::social_posts_replaced_by::TABLE)?;
        let mentions = tx.open_table(&crate::social_posts_self_mention::TABLE)?;
        let end = after.map_or(Bound::Unbounded, |c| Bound::Excluded((c.ts, c.seq)));
        let mut last = after;
        for entry in receipts.range((Bound::Unbounded, end))?.rev() {
            if page.rows.len() == bounds.rows {
                page.stop = NotificationStop::PageLimit;
                break;
            }
            if page.raw_rows_scanned == bounds.raw_rows {
                page.stop = NotificationStop::ScanBudget;
                break;
            }
            let (key, id) = entry?;
            let (ts, seq) = key.value_try()?;
            let received = ReceivedAtPaginationCursor { ts, seq };
            let event_id = id.value_try()?;
            // Advance before cheap skips; a byte-boundary stop restores this.
            let previous = last;
            last = Some(received);
            page.raw_rows_scanned += 1;
            if seq < minimum_sequence || seq >= snapshot.next_sequence {
                continue;
            }
            let Some(event) = events
                .get(&event_id)?
                .map(|row| row.value_try())
                .transpose()?
            else {
                return Err(DbError::NotificationInvariant);
            };
            if event.event().compute_id().to_short() != event_id
                || event.kind() != EventKind::SOCIAL_POST
            {
                return Err(DbError::NotificationInvariant);
            }
            if event.author() == self.self_id
                || Self::is_social_post_replaced_tx(event.author(), event_id, &replacements)?
                || states.get(&event_id)?.is_some()
            {
                continue;
            }
            let mention = mentions.get(&event_id)?.is_some();
            let target = (event.author(), event.event().compute_id());
            let encoded_bytes = event.content_len();
            // Borrow and validate the encoded store length before any owned decode.
            let encoded_key =
                crate::payload_admission::encode_content_hash_key(event.content_hash());
            let Some(raw) = store.as_raw().get(encoded_key.as_slice())? else {
                continue;
            };
            crate::payload_admission::decode_stored_payload_after_length_guard(
                raw.value(),
                encoded_bytes,
                |_| (),
            )?;
            if encoded_bytes as usize > bounds.payload_bytes {
                if mention {
                    page.rows.push(NotificationRow {
                        target,
                        received,
                        content: None,
                        encoded_bytes,
                        reason: NotificationReason {
                            reply: None,
                            mention,
                        },
                    });
                } else {
                    page.issues.push(NotificationIssue {
                        target,
                        received,
                        encoded_bytes,
                    });
                }
                continue;
            }
            if page.payload_bytes_read + encoded_bytes as usize > bounds.payload_bytes {
                last = previous;
                page.raw_rows_scanned -= 1;
                page.stop = NotificationStop::PayloadBudget;
                break;
            }
            page.payload_bytes_read += encoded_bytes as usize;
            let stored = store
                .get(&event.content_hash())?
                .ok_or(DbError::NotificationInvariant)?
                .value_try()?;
            let Ok(content) = stored.0.deserialize_cbor::<SocialPost>() else {
                continue;
            };
            let reply = content
                .reply_to
                .is_some_and(|id| id.rostra_id() == self.self_id);
            if reply || mention {
                page.rows.push(NotificationRow {
                    target,
                    received,
                    content: Some(content),
                    encoded_bytes,
                    reason: NotificationReason {
                        reply: Some(reply),
                        mention,
                    },
                });
            }
        }
        if page.stop != NotificationStop::End {
            page.next_cursor = Some(NotificationCursor {
                snapshot: snapshot.clone(),
                position: last.ok_or(DbError::NotificationInvariant)?,
            });
        }
        Ok(page)
    }
}
