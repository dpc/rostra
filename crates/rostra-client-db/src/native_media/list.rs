//! Bounded own-author retained candidates, never a decoded-image safety claim.

use std::ops::Bound;

use rostra_core::avatar::{MAX_AVATAR_DIMENSION, MAX_AVATAR_PIXELS};
use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::{ContentHash, EventId, ShortEventId};

use super::{ImagePolicy, MAX_NATIVE_MEDIA_BODY_BYTES, decode, payload};
use crate::{Database, DbError, DbResult, EventRecord};

/// Exclusive live traversal position, invalidated by reopen/projection rebuild.
///
/// Not a snapshot: concurrent new lower keys may require an explicit refresh.
#[derive(Debug, Clone)]
pub struct OwnMediaCursor {
    /// Account owning the traversal.
    account: RostraId,
    /// Per-open instance token.
    instance: [u8; 16],
    /// Persistent projection/rebuild lifetime.
    epoch: [u8; 16],
    /// Last examined self-index key.
    position: ShortEventId,
    /// Admission policy that created this traversal.
    jpeg: bool,
}

/// Explicitly unvalidated pixel eligibility for a retained image candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnMediaEligibility {
    /// Header, signature and payload authenticate; pixels still need
    /// read_native_media.
    RequiresReadValidation,
}

/// Authenticated retained metadata, not permission to display or insert
/// blindly.
#[derive(Debug, Clone)]
pub struct OwnMediaCandidate {
    /// Exact full signed event identity.
    pub event_id: EventId,
    /// Exact policy-admitted MIME declaration matched to an encoded header.
    pub mime: &'static str,
    /// Unvalidated encoded header width.
    pub width: u32,
    /// Unvalidated encoded header height.
    pub height: u32,
    /// Encoded image byte count from complete CBOR preflight.
    pub bytes: usize,
    /// A tap must perform exact-author/full-ID read validation before reuse.
    pub eligibility: OwnMediaEligibility,
}

/// At most twenty candidates; an empty page can still have a continuation.
#[derive(Debug)]
pub struct OwnMediaPage {
    /// Candidate metadata only; no payload copies or pixel decoding.
    pub items: Vec<OwnMediaCandidate>,
    /// Continue even when this page contains no candidates.
    pub next_cursor: Option<OwnMediaCursor>,
    /// Number of raw own-index rows examined, capped at 128.
    pub raw_rows_scanned: usize,
    /// Full body reservations, capped at eight maximum-sized bodies.
    pub bytes_reserved: usize,
}

impl Database {
    /// List locally retained own-author image candidates without fetching.
    ///
    /// Uses the self-event index, raw fallible iteration and checked values.
    /// Charges one maximum body reservation per examined row before reading;
    /// at most eight rows/16,001,024 body bytes are inspected per call (the
    /// separate raw-row ceiling is 128). Oversize/unsupported/corrupt records
    /// are skipped with progress; malformed index keys fail explicitly because
    /// they cannot be represented by a safe continuation. Storage errors
    /// propagate. No full payload vector, codec or whole-collection scan.
    pub async fn list_own_media(&self, cursor: Option<&OwnMediaCursor>) -> DbResult<OwnMediaPage> {
        self.list_own_media_with_policy(cursor, ImagePolicy::Default)
            .await
    }

    /// List metadata-only PNG/WebP and restricted JPEG candidates.
    ///
    /// Same row/body budgets and authentication as `list_own_media`; JPEG uses
    /// a bounded borrowed CBOR/header walk, never pixel decoding or a payload
    /// copy. Eligibility remains RequiresReadValidation: preview/insertion must
    /// call `read_native_media_with_jpeg` with the exact account and full ID.
    /// Listing itself needs no retained-validation permit. Cursors cannot be
    /// exchanged with the default policy.
    #[cfg(feature = "native-media-jpeg")]
    pub async fn list_own_media_with_jpeg(
        &self,
        cursor: Option<&OwnMediaCursor>,
    ) -> DbResult<OwnMediaPage> {
        self.list_own_media_with_policy(cursor, ImagePolicy::Jpeg)
            .await
    }

    async fn list_own_media_with_policy(
        &self,
        cursor: Option<&OwnMediaCursor>,
        policy: ImagePolicy,
    ) -> DbResult<OwnMediaPage> {
        let jpeg = !matches!(policy, ImagePolicy::Default);
        self.read_with(|tx| {
            let epoch = tx
                .open_table(&crate::social_notification_epoch::TABLE)?
                .get(&())?
                .ok_or(DbError::MediaCandidate)?
                .value_try()?;
            if let Some(cursor) = cursor
                && (cursor.account != self.self_id
                    || cursor.instance != self.news_instance
                    || cursor.epoch != epoch
                    || cursor.jpeg != jpeg)
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
            let mut page = OwnMediaPage {
                items: Vec::new(),
                next_cursor: None,
                raw_rows_scanned: 0,
                bytes_reserved: 0,
            };
            let mut position = None;
            loop {
                if page.items.len() == 20
                    || page.raw_rows_scanned == 128
                    || page.bytes_reserved == 8 * MAX_NATIVE_MEDIA_BODY_BYTES
                {
                    page.next_cursor = position.map(|position| OwnMediaCursor {
                        account: self.self_id,
                        instance: self.news_instance,
                        epoch,
                        position,
                        jpeg,
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
                let Ok((mime, encoded, bytes)) = payload::preflight_with_policy(body, policy)
                else {
                    continue;
                };
                let Some((width, height)) = candidate_dimensions(mime, encoded, bytes) else {
                    continue;
                };
                if width == 0
                    || height == 0
                    || width > MAX_AVATAR_DIMENSION
                    || height > MAX_AVATAR_DIMENSION
                    || u64::from(width) * u64::from(height) > MAX_AVATAR_PIXELS
                {
                    continue;
                }
                page.items.push(OwnMediaCandidate {
                    event_id: event.signed.compute_id(),
                    mime,
                    width,
                    height,
                    bytes,
                    eligibility: OwnMediaEligibility::RequiresReadValidation,
                });
            }
            Ok(page)
        })
        .await
    }
}

fn candidate_dimensions(mime: &str, encoded: &[u8], bytes: usize) -> Option<(u32, u32)> {
    #[cfg(feature = "native-media-jpeg")]
    if mime == "image/jpeg" {
        return super::jpeg_header::dimensions(encoded, bytes);
    }
    let prefix = payload::prefix(encoded, bytes).ok()?;
    dimensions(mime, &prefix, bytes)
}

fn dimensions(mime: &str, bytes: &[u8; 30], len: usize) -> Option<(u32, u32)> {
    if mime == "image/png" {
        if len < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[8..16] != b"\0\0\0\rIHDR" {
            return None;
        }
        return Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        ));
    }
    if len < 25
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WEBP"
        || u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize + 8 != len
    {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" if len >= 30 && bytes[20] & 2 == 0 => {
            let width = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]) + 1;
            let height = u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]) + 1;
            Some((width, height))
        }
        b"VP8 " if len >= 30 && &bytes[23..26] == b"\x9d\x01\x2a" => Some((
            u32::from(u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3fff),
            u32::from(u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3fff),
        )),
        b"VP8L" if bytes[20] == 0x2f => {
            let bits = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        _ => None,
    }
}
