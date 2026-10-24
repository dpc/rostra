//! Checked raw range/point reads, avoiding typed range key-decode panics.

use std::ops::Bound;

use redb_bincode::ReadTransaction;
use rostra_core::id::RostraId;

use crate::relationship::{
    RELATIONSHIP_BYTES_MIN, RelationshipBounds, RelationshipCursor, RelationshipDirection,
    RelationshipPage, RelationshipRow, RelationshipState, RelationshipStop,
};
use crate::relationship_selector::{decode_exact, selector};
use crate::{Database, DbError, DbResult, ids_followees, ids_followers};

fn encoded<T: bincode::Encode>(value: T) -> DbResult<Vec<u8>> {
    bincode::encode_to_vec(value, redb_bincode::BINCODE_CONFIG)
        .map_err(|_| DbError::RelationshipInvariant)
}

pub(crate) fn own_follow(
    followees: &redb_bincode::ReadOnlyTable<ids_followees::Key, ids_followees::Value>,
    owner: RostraId,
    identity: RostraId,
) -> DbResult<(RelationshipState, usize)> {
    let key = encoded((owner, identity))?;
    let Some(value) = followees.as_raw().get(key.as_slice())? else {
        return Ok((RelationshipState::NotFollowing, key.len()));
    };
    let (selector, bytes) = selector(value.value())?;
    Ok((RelationshipState::Following { selector }, key.len() + bytes))
}

fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    let index = end.iter().rposition(|byte| *byte != u8::MAX)?;
    end[index] += 1;
    end.truncate(index + 1);
    Some(end)
}

impl Database {
    pub(crate) fn relationship_page_tx(
        &self,
        tx: &ReadTransaction,
        direction: RelationshipDirection,
        cursor: Option<&RelationshipCursor>,
        bounds: RelationshipBounds,
    ) -> DbResult<RelationshipPage> {
        let epoch = tx
            .open_table(&crate::social_notification_epoch::TABLE)?
            .get(&())?
            .ok_or(DbError::RelationshipInvariant)?
            .value_try()?;
        if let Some(cursor) = cursor
            && (cursor.account != self.self_id
                || cursor.direction != direction
                || cursor.epoch != epoch
                || cursor.instance != self.news_instance)
        {
            return Err(DbError::RelationshipCursorStale);
        }
        let followees = tx.open_table(&ids_followees::TABLE)?;
        let followers = tx.open_table(&ids_followers::TABLE)?;
        let index = match direction {
            RelationshipDirection::Following => followees.as_raw(),
            RelationshipDirection::Followers => followers.as_raw(),
        };
        let prefix = encoded(self.self_id)?;
        let end = prefix_end(&prefix);
        let after = cursor
            .map(|cursor| encoded((self.self_id, cursor.position)))
            .transpose()?;
        let start = after
            .as_deref()
            .map_or(Bound::Included(prefix.as_slice()), Bound::Excluded);
        let mut iter = index.range::<&[u8]>((
            start,
            end.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
        ))?;
        let mut page = RelationshipPage {
            rows: Vec::new(),
            next_cursor: None,
            stop: RelationshipStop::End,
            raw_rows_scanned: 0,
            point_reads: 0,
            bytes_charged: 0,
        };
        loop {
            // Check before advancing the iterator: no extra lookahead read or
            // typed key validation can escape the per-call row/byte bounds.
            if page.rows.len() == bounds.rows {
                page.stop = RelationshipStop::PageLimit;
                break;
            }
            if bounds.bytes - page.bytes_charged < RELATIONSHIP_BYTES_MIN {
                page.stop = RelationshipStop::ByteBudget;
                break;
            }
            let Some(entry) = iter.next() else {
                break;
            };
            let (key, value) = entry?;
            // Full identities have fixed 32-byte encodings. Check before decode,
            // including corrupt keys and malformed keys at prefix boundaries.
            if key.value().len() != 64 {
                return Err(DbError::RelationshipInvariant);
            }
            let (owner, identity): (RostraId, RostraId) = decode_exact(key.value())?;
            if owner != self.self_id {
                return Err(DbError::RelationshipInvariant);
            }
            page.raw_rows_scanned += 1;
            page.bytes_charged += key.value().len();
            let own_follow = match direction {
                RelationshipDirection::Following => {
                    let (selector, bytes) = selector(value.value())?;
                    page.bytes_charged += bytes;
                    RelationshipState::Following { selector }
                }
                RelationshipDirection::Followers => {
                    // The reverse record is an empty struct. Any stored bytes
                    // are corruption, including future unsupported encodings.
                    if !value.value().is_empty() {
                        return Err(DbError::RelationshipInvariant);
                    }
                    let (state, bytes) = own_follow(&followees, self.self_id, identity)?;
                    page.point_reads += 1;
                    page.bytes_charged += bytes;
                    state
                }
            };
            page.rows.push(RelationshipRow {
                identity,
                own_follow,
            });
        }
        if page.stop != RelationshipStop::End {
            let position = page
                .rows
                .last()
                .ok_or(DbError::RelationshipInvariant)?
                .identity;
            page.next_cursor = Some(RelationshipCursor {
                account: self.self_id,
                direction,
                epoch,
                instance: self.news_instance,
                position,
            });
        }
        Ok(page)
    }
}
