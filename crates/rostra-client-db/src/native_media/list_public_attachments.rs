//! Bounded metadata for reusing retained own opaque attachments, not opening
//! them.

use std::ops::Bound;

use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::{ContentHash, EventId, ShortEventId};

use super::{MAX_NATIVE_MEDIA_BODY_BYTES, ReadPolicy, decode, payload};
use crate::public_attachment_request::{PUBLIC_ATTACHMENT_MAX_BYTES, PUBLIC_ATTACHMENT_MIME};
use crate::{Database, DbError, DbResult, EventRecord};

/// Exclusive live position, bound to the account, database open and rebuild
/// epoch.
///
/// This is not a snapshot or a serializable host token. Refresh to see
/// concurrent insertions before the last examined key. Hosts must bound
/// retained cursors.
#[derive(Debug, Clone)]
pub struct OwnPublicAttachmentCursor {
    /// Account owning the traversal.
    account: RostraId,
    /// Per-open instance token.
    instance: [u8; 16],
    /// Persistent projection/rebuild lifetime.
    epoch: [u8; 16],
    /// Last examined canonical self-index key.
    position: ShortEventId,
}

/// Authenticated own-author metadata, not current insertion or safe-open
/// authority.
///
/// The retained signed envelope authenticates the database owner's full author
/// ID and this full event ID; the index alone is never ownership evidence.
/// Before insertion, call `read_native_media_opaque` for that owner and exact
/// full ID, requiring Available, the same author/ID/MIME and 1..=1,000,000
/// original bytes. Later display/export also needs a fresh lifecycle read,
/// never a list cache.
#[derive(Debug, Clone)]
pub struct OwnPublicAttachmentCandidate {
    /// Exact full identity recomputed from the verified retained signed
    /// envelope.
    pub event_id: EventId,
    /// Exact `application/octet-stream` declaration, not a content safety
    /// claim.
    pub mime: &'static str,
    /// Original data length, not signed CBOR length; always 1..=1,000,000.
    pub bytes: usize,
}

/// Why this live traversal call stopped, without an uncharged lookahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnPublicAttachmentStop {
    /// The current own-index iterator reached its end.
    Exhausted,
    /// A call budget was reached; continuation may itself return an empty end.
    BudgetReached,
}

/// Metadata-only bounded progress; an empty page may still require
/// continuation.
#[derive(Debug)]
pub struct OwnPublicAttachmentPage {
    /// At most twenty candidates (the eight-row reservation cap is tighter).
    pub items: Vec<OwnPublicAttachmentCandidate>,
    /// Exclusive continuation; present exactly when the stop is BudgetReached.
    pub next_cursor: Option<OwnPublicAttachmentCursor>,
    /// Raw own-index rows examined, including other kinds; at most eight
    /// currently.
    pub raw_rows_scanned: usize,
    /// Conservative body reservations: at most 8 * 2,000,128 encoded bytes.
    pub bytes_reserved: usize,
    /// Examined rows not returned, including nonmedia, unavailable and bad
    /// records.
    pub skipped_rows: usize,
    /// Exhaustion versus an explicit bounded-work stop.
    pub stop: OwnPublicAttachmentStop,
}

impl Database {
    /// List retained own public opaque attachment metadata without payload
    /// copies.
    ///
    /// Separate from strict PNG/WebP `list_own_media`. Only exact
    /// `application/octet-stream` with nonempty original data up to 1,000,000
    /// bytes passes the complete default-policy CBOR preflight. No
    /// normalization, pixel decoding, fetching, scheduling, signing, writes
    /// or schema changes.
    ///
    /// Each raw own-index row reserves a maximum 2,000,128-byte signed body
    /// before inspection, regardless of kind/availability. Eight
    /// reservations limit work more tightly than the 128 raw-row and
    /// twenty-output ceilings. Each raw stored body also has a nine-byte
    /// wrapper allowance; envelopes are at most 1024 bytes. Hashing and
    /// CBOR traversal borrow only checked, bounded bodies. Bounds are not a
    /// CPU deadline or process RSS guarantee; hosts bound concurrency and
    /// keep this off their UI executor.
    ///
    /// Unavailable, unsupported, oversized or corrupt candidates count as
    /// skipped, not necessarily invalid protocol content. Backend errors
    /// propagate; malformed/noncanonical index keys or values fail with
    /// MediaCandidate rather than manufacture unsafe progress.
    /// Foreign/open/rebuild cursors fail with MediaCursorStale,
    /// consistently with the image list.
    pub async fn list_own_public_attachments(
        &self,
        cursor: Option<&OwnPublicAttachmentCursor>,
    ) -> DbResult<OwnPublicAttachmentPage> {
        self.read_with(|tx| {
            let epoch = tx
                .open_table(&crate::social_notification_epoch::TABLE)?
                .get(&())?
                .ok_or(DbError::MediaCandidate)?
                .value_try()?;
            if let Some(cursor) = cursor
                && (cursor.account != self.self_id
                    || cursor.instance != self.news_instance
                    || cursor.epoch != epoch)
            {
                return Err(DbError::MediaCursorStale);
            }
            let index = tx.open_table(&crate::events_self::TABLE)?;
            let events = tx.open_table(&crate::events::TABLE)?;
            let states = tx.open_table(&crate::events_content_state::TABLE)?;
            let store = tx.open_table(&crate::content_store::TABLE)?;
            let start = cursor.map(|c| {
                bincode::encode_to_vec(c.position, redb_bincode::BINCODE_CONFIG).expect("fixed key")
            });
            let bound = start
                .as_deref()
                .map(Bound::Excluded)
                .unwrap_or(Bound::Unbounded);
            let mut iter = index.as_raw().range::<&[u8]>((bound, Bound::Unbounded))?;
            let mut page = OwnPublicAttachmentPage {
                items: Vec::new(),
                next_cursor: None,
                raw_rows_scanned: 0,
                bytes_reserved: 0,
                skipped_rows: 0,
                stop: OwnPublicAttachmentStop::Exhausted,
            };
            let mut position = None;
            loop {
                if page.items.len() == 20
                    || page.raw_rows_scanned == 128
                    || page.bytes_reserved == 8 * MAX_NATIVE_MEDIA_BODY_BYTES
                {
                    page.stop = OwnPublicAttachmentStop::BudgetReached;
                    page.next_cursor = position.map(|position| OwnPublicAttachmentCursor {
                        account: self.self_id,
                        instance: self.news_instance,
                        epoch,
                        position,
                    });
                    break;
                }
                let Some(row) = iter.next() else { break };
                let (key, value) = row?;
                page.raw_rows_scanned += 1;
                page.bytes_reserved += MAX_NATIVE_MEDIA_BODY_BYTES;
                let id: ShortEventId = decode(key.value(), 32).ok_or(DbError::MediaCandidate)?;
                if bincode::encode_to_vec(id, redb_bincode::BINCODE_CONFIG).expect("fixed key")
                    != key.value()
                    || !value.value().is_empty()
                {
                    return Err(DbError::MediaCandidate);
                }
                position = Some(id);
                let Some(raw) = events.as_raw().get(key.value())? else {
                    continue;
                };
                let Some(event) = decode::<EventRecord>(raw.value(), 1024) else {
                    continue;
                };
                if event.signed.compute_short_id() != id
                    || event.author() != self.self_id
                    || event.kind() != EventKind::SOCIAL_MEDIA
                    || VerifiedEvent::verify_signed(self.self_id, event.signed).is_err()
                    || event.content_len() as usize > MAX_NATIVE_MEDIA_BODY_BYTES
                {
                    continue;
                }
                // Any lifecycle row means not currently retained/present.
                if states.as_raw().get(key.value())?.is_some() {
                    continue;
                }
                let hash_key =
                    bincode::encode_to_vec(event.content_hash(), redb_bincode::BINCODE_CONFIG)
                        .expect("fixed key");
                let Some(raw) = store.as_raw().get(hash_key.as_slice())? else {
                    continue;
                };
                if raw.value().len() > MAX_NATIVE_MEDIA_BODY_BYTES + 9 {
                    continue;
                }
                let Ok((body, consumed)): Result<(&[u8], usize), _> =
                    bincode::borrow_decode_from_slice(raw.value(), redb_bincode::BINCODE_CONFIG)
                else {
                    continue;
                };
                if consumed != raw.value().len()
                    || body.len() != event.content_len() as usize
                    || ContentHash::from(blake3::hash(body)) != event.content_hash()
                {
                    continue;
                }
                let Ok((mime, _, bytes)) = payload::preflight_opaque(body, ReadPolicy::Default)
                else {
                    continue;
                };
                if mime != PUBLIC_ATTACHMENT_MIME
                    || !(1..=PUBLIC_ATTACHMENT_MAX_BYTES).contains(&bytes)
                {
                    continue;
                }
                page.items.push(OwnPublicAttachmentCandidate {
                    event_id: event.signed.compute_id(),
                    mime: PUBLIC_ATTACHMENT_MIME,
                    bytes,
                });
            }
            page.skipped_rows = page.raw_rows_scanned - page.items.len();
            Ok(page)
        })
        .await
    }
}
