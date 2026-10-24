//! Checked raw reads and bounded payload admission for author tag discovery.

use std::collections::BTreeSet;
use std::ops::Bound;

use redb_bincode::ReadTransaction;
use rostra_core::event::content_kind::discovered_tags::{
    self, PAYLOAD_BYTES_MAX, TAG_ELEMENTS_MAX, TagDiscoveryIssue,
};
use rostra_core::event::{EventExt as _, EventKind};
use rostra_core::id::RostraId;
use rostra_core::{ContentHash, ShortEventId, Timestamp};

use crate::author_tags::{
    AUTHOR_TAG_BYTES_MIN, AUTHOR_TAG_WORK_MIN, AuthorTagBounds, AuthorTagCursor, AuthorTagPage,
    AuthorTagSkipped, AuthorTagStop,
};
use crate::{Database, DbError, DbResult, EventRecord};

fn encoded<T: bincode::Encode>(value: T) -> DbResult<Vec<u8>> {
    bincode::encode_to_vec(value, redb_bincode::BINCODE_CONFIG)
        .map_err(|_| DbError::AuthorTagInvariant)
}

fn decode<T: bincode::Decode<()>>(raw: &[u8], limit: usize) -> DbResult<T> {
    if raw.len() > limit {
        return Err(DbError::AuthorTagInvariant);
    }
    let (value, consumed) = bincode::decode_from_slice(raw, redb_bincode::BINCODE_CONFIG)?;
    if consumed != raw.len() {
        return Err(DbError::AuthorTagInvariant);
    }
    Ok(value)
}

impl Database {
    pub(crate) fn author_tags_page_tx(
        &self,
        tx: &ReadTransaction,
        author: RostraId,
        cursor: Option<&AuthorTagCursor>,
        bounds: AuthorTagBounds,
    ) -> DbResult<AuthorTagPage> {
        let epoch = tx
            .open_table(&crate::social_notification_epoch::TABLE)?
            .get(&())?
            .ok_or(DbError::AuthorTagInvariant)?
            .value_try()?;
        if let Some(cursor) = cursor
            && (cursor.account != self.self_id
                || cursor.author != author
                || cursor.epoch != epoch
                || cursor.instance != self.news_instance
                || cursor.offset >= TAG_ELEMENTS_MAX)
        {
            return Err(DbError::AuthorTagCursorStale);
        }
        let index = tx.open_table(&crate::social_posts_by_time::TABLE)?;
        let events = tx.open_table(&crate::events::TABLE)?;
        let states = tx.open_table(&crate::events_content_state::TABLE)?;
        let store = tx.open_table(&crate::content_store::TABLE)?;
        let start = cursor.map(|cursor| encoded(cursor.position)).transpose()?;
        let end = match (start.as_deref(), cursor) {
            (Some(key), Some(cursor)) if cursor.offset > 0 => Bound::Included(key),
            (Some(key), _) => Bound::Excluded(key),
            _ => Bound::Unbounded,
        };
        let mut iter = index
            .as_raw()
            .range::<&[u8]>((Bound::Unbounded, end))?
            .rev();
        let mut page = AuthorTagPage {
            tags: BTreeSet::new(),
            next_cursor: None,
            stop: AuthorTagStop::End,
            raw_rows_scanned: 0,
            bytes_reserved: 0,
            work_reserved: 0,
            skipped: AuthorTagSkipped::default(),
        };
        let mut position = None;
        loop {
            // No lookahead: even unrelated, missing and repeated-tag rows consume
            // one full reservation before any record is examined.
            if page.tags.len() == bounds.tags {
                page.stop = AuthorTagStop::Tags;
                break;
            }
            if page.raw_rows_scanned == bounds.raw_rows {
                page.stop = AuthorTagStop::Rows;
                break;
            }
            if bounds.bytes - page.bytes_reserved < AUTHOR_TAG_BYTES_MIN {
                page.stop = AuthorTagStop::Bytes;
                break;
            }
            if bounds.work - page.work_reserved < AUTHOR_TAG_WORK_MIN {
                page.stop = AuthorTagStop::Work;
                break;
            }
            let Some(entry) = iter.next() else {
                break;
            };
            let (key, value) = entry?;
            let row: (Timestamp, ShortEventId) = decode(key.value(), 32)?;
            // The continuation re-encodes this position. A nonminimal varint
            // could otherwise move its raw seek boundary and silently lose rows.
            if encoded(row)?.as_slice() != key.value() || !value.value().is_empty() {
                return Err(DbError::AuthorTagInvariant);
            }
            page.raw_rows_scanned += 1;
            page.bytes_reserved += AUTHOR_TAG_BYTES_MIN;
            page.work_reserved += AUTHOR_TAG_WORK_MIN;
            position = Some((row, 0));

            let event_key = encoded(row.1)?;
            let Some(raw) = events.as_raw().get(event_key.as_slice())? else {
                page.skipped.unavailable += 1;
                continue;
            };
            let event: EventRecord = decode(raw.value(), 1024)?;
            if event.event().compute_short_id() != row.1
                || event.timestamp() != row.0
                || event.kind() != EventKind::SOCIAL_POST
            {
                return Err(DbError::AuthorTagInvariant);
            }
            if event.author() != author {
                continue;
            }
            if let Some(raw) = states.as_raw().get(event_key.as_slice())? {
                let _: crate::event::EventContentState = decode(raw.value(), 64)?;
                page.skipped.unavailable += 1;
                continue;
            }
            let content_key = encoded(event.content_hash())?;
            let Some(raw) = store.as_raw().get(content_key.as_slice())? else {
                page.skipped.unavailable += 1;
                continue;
            };
            // A bincode byte-slice wrapper has at most nine prefix bytes. Do not
            // decode an owned ContentStoreRecord or trust a forged Vec length.
            if raw.value().len() > PAYLOAD_BYTES_MAX + 9 {
                page.skipped.oversized += 1;
                continue;
            }
            let (payload, consumed): (&[u8], usize) =
                bincode::borrow_decode_from_slice(raw.value(), redb_bincode::BINCODE_CONFIG)?;
            if consumed != raw.value().len()
                || payload.len() != event.content_len() as usize
                || ContentHash::from(blake3::hash(payload)) != event.content_hash()
            {
                return Err(DbError::AuthorTagInvariant);
            }
            let tags = match discovered_tags::decode(payload) {
                Ok(tags) => tags,
                Err(TagDiscoveryIssue::Oversized) => {
                    page.skipped.oversized += 1;
                    continue;
                }
                Err(TagDiscoveryIssue::Unsupported) => {
                    page.skipped.unsupported += 1;
                    continue;
                }
            };
            let offset = cursor
                .filter(|cursor| cursor.position == row)
                .map_or(0, |cursor| cursor.offset);
            if offset > tags.len() {
                return Err(DbError::AuthorTagCursorStale);
            }
            let count = tags.len();
            for (index, tag) in tags.into_iter().enumerate().skip(offset) {
                page.tags.insert(tag);
                if page.tags.len() == bounds.tags {
                    if index + 1 < count {
                        position = Some((row, index + 1));
                    }
                    break;
                }
            }
        }
        if page.stop != AuthorTagStop::End {
            let (position, offset) = position.ok_or(DbError::AuthorTagInvariant)?;
            page.next_cursor = Some(AuthorTagCursor {
                account: self.self_id,
                author,
                epoch,
                instance: self.news_instance,
                position,
                offset,
            });
        }
        Ok(page)
    }
}
