//! Checked raw-index traversal; no generic unbounded filtered paginator.

use std::ops::Bound;

use redb_bincode::ReadTransaction;
use rostra_core::event::content_kind::Shoutbox;
use rostra_core::event::{EventExt as _, EventKind};
use rostra_core::id::ToShort as _;

use crate::shoutbox::{
    SHOUTBOX_CONTENT_MAX, ShoutboxBounds, ShoutboxCursor, ShoutboxIssue, ShoutboxPage, ShoutboxRow,
    ShoutboxStop,
};
use crate::social::ReceivedAtPaginationCursor;
use crate::{Database, DbError, DbResult};

/// Decode fixed-size index fields without the typed iterator's infallible
/// guard.
fn decode_index_field<T: bincode::Decode<()>>(bytes: &[u8]) -> DbResult<T> {
    let (value, consumed) = bincode::decode_from_slice(bytes, redb_bincode::BINCODE_CONFIG)?;
    if consumed != bytes.len() {
        return Err(DbError::ShoutboxInvariant);
    }
    Ok(value)
}

impl Database {
    pub(crate) fn scan_shoutbox_tx(
        &self,
        tx: &ReadTransaction,
        cursor: Option<&ShoutboxCursor>,
        bounds: ShoutboxBounds,
    ) -> DbResult<ShoutboxPage> {
        let epoch = tx
            .open_table(&crate::social_notification_epoch::TABLE)?
            .get(&())?
            .ok_or(DbError::ShoutboxInvariant)?
            .value_try()?;
        let current_next = tx
            .open_table(&crate::reception_order_next::TABLE)?
            .get(&())?
            .map(|row| row.value_try())
            .transpose()?
            .unwrap_or(0);
        if let Some(cursor) = cursor
            && (cursor.account != self.self_id
                || cursor.epoch != epoch
                || cursor.instance != self.news_instance
                || cursor.next_sequence > current_next)
        {
            return Err(DbError::ShoutboxCursorStale);
        }
        let next_sequence = cursor.map_or(current_next, |cursor| cursor.next_sequence);
        let after = cursor.map(|cursor| cursor.position);
        let mut page = ShoutboxPage {
            rows: Vec::new(),
            next_cursor: None,
            stop: ShoutboxStop::End,
            raw_rows_scanned: 0,
            payload_bytes_read: 0,
            unavailable_rows: 0,
        };
        let index = tx.open_table(&crate::shoutbox_posts_by_received_at::TABLE)?;
        let events = tx.open_table(&crate::events::TABLE)?;
        let states = tx.open_table(&crate::events_content_state::TABLE)?;
        let store = tx.open_table(&crate::content_store::TABLE)?;
        let encoded_end = after
            .map(|c| {
                bincode::encode_to_vec((c.ts, c.seq), redb_bincode::BINCODE_CONFIG)
                    .map_err(|_| DbError::ShoutboxInvariant)
            })
            .transpose()?;
        let end = encoded_end
            .as_deref()
            .map_or(Bound::Unbounded, Bound::Excluded);
        let mut last = after;
        // redb-bincode 0.5's typed range validates keys with expect before
        // returning them. Raw byte ordering plus exact decoding stays fallible.
        for entry in index
            .as_raw()
            .range::<&[u8]>((Bound::Unbounded, end))?
            .rev()
        {
            if page.rows.len() == bounds.rows {
                page.stop = ShoutboxStop::PageLimit;
                break;
            }
            if page.raw_rows_scanned == bounds.raw_rows {
                page.stop = ShoutboxStop::ScanBudget;
                break;
            }
            let (key, id) = entry?;
            let (ts, seq) = decode_index_field(key.value())?;
            let event_id: rostra_core::ShortEventId = decode_index_field(id.value())?;
            let received = ReceivedAtPaginationCursor { ts, seq };
            let previous = last;
            last = Some(received);
            page.raw_rows_scanned += 1;
            if seq >= current_next {
                return Err(DbError::ShoutboxInvariant);
            }
            if seq >= next_sequence {
                continue;
            }
            let event = events
                .get(&event_id)?
                .map(|row| row.value_try())
                .transpose()?
                .ok_or(DbError::ShoutboxInvariant)?;
            if event.event().compute_id().to_short() != event_id
                || event.kind() != EventKind::SHOUTBOX
            {
                return Err(DbError::ShoutboxInvariant);
            }
            if states
                .get(&event_id)?
                .map(|row| row.value_try())
                .transpose()?
                .is_some()
            {
                page.unavailable_rows += 1;
                continue;
            }
            let encoded_bytes = event.content_len();
            let encoded_key =
                crate::payload_admission::encode_content_hash_key(event.content_hash());
            let Some(raw) = store.as_raw().get(encoded_key.as_slice())? else {
                page.unavailable_rows += 1;
                continue;
            };
            // Borrow the stored record to validate length before owned decoding.
            crate::payload_admission::decode_stored_payload_after_length_guard(
                raw.value(),
                encoded_bytes,
                |_| (),
            )?;
            let oversized = encoded_bytes as usize > bounds.payload_bytes
                || encoded_bytes as usize > SHOUTBOX_CONTENT_MAX;
            let (content, issue) = if oversized {
                (None, Some(ShoutboxIssue::Oversized))
            } else {
                if page.payload_bytes_read + encoded_bytes as usize > bounds.payload_bytes {
                    last = previous;
                    page.raw_rows_scanned -= 1;
                    page.stop = ShoutboxStop::PayloadBudget;
                    break;
                }
                page.payload_bytes_read += encoded_bytes as usize;
                let content = crate::payload_admission::decode_stored_payload_after_length_guard(
                    raw.value(),
                    encoded_bytes,
                    |bytes| {
                        let hash: rostra_core::ContentHash = blake3::hash(bytes).into();
                        if hash != event.content_hash() {
                            return Err(DbError::ShoutboxInvariant);
                        }
                        Ok(Shoutbox::decode_supported(bytes))
                    },
                )??;
                let issue = content.is_none().then_some(ShoutboxIssue::Unsupported);
                (content, issue)
            };
            page.rows.push(ShoutboxRow {
                target: (event.author(), event.event().compute_id()),
                received,
                timestamp: event.timestamp(),
                content,
                issue,
                encoded_bytes,
            });
        }
        if page.stop != ShoutboxStop::End {
            page.next_cursor = Some(ShoutboxCursor {
                account: self.self_id,
                epoch,
                instance: self.news_instance,
                next_sequence,
                position: last.ok_or(DbError::ShoutboxInvariant)?,
            });
        }
        Ok(page)
    }
}
