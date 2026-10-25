//! Opt-in, local-only signed static media presentation and opaque export.
//!
//! The caller supplies the actual retained source post's author, not an
//! identity inferred from a caption or untrusted UI argument. Web Djot media is
//! scoped to that author; cross-author reuse is not authorized. Full IDs must
//! match exactly. Availability describes one read snapshot, not a permanent
//! cache capability. Re-read on later presentation; deletion/pruning must not
//! be bypassed by a host cache. No network request, fetch scheduling, signing,
//! or write occurs here.

use rostra_core::avatar::{RasterAvatarError, validate_raster_avatar};
use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::media::MediaEventReference;
use rostra_core::{ContentHash, EventId};

use crate::{Database, DbResult, EventRecord};

mod list;
mod opaque;
mod payload;
pub use list::{OwnMediaCandidate, OwnMediaCursor, OwnMediaEligibility, OwnMediaPage};
pub use opaque::OpaqueMediaRead;
#[cfg(test)]
mod tests;

/// Maximum accepted CBOR body, distinct from the 1,000,000 media-byte limit.
///
/// Canonical `Vec<u8>` uses at most two CBOR bytes per byte, plus bounded map,
/// key, MIME and array headers. Wider integer encodings are Unsupported, not
/// invalid protocol data. This is a native read policy, not a protocol limit.
pub const MAX_NATIVE_MEDIA_BODY_BYTES: usize = 2_000_128;

/// A local read outcome; no unavailable variant contains media bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeMediaRead {
    /// Authenticated original bytes accepted by the static raster policy.
    Available {
        /// Exact signed media author, equal to the supplied source author.
        author: RostraId,
        /// Recomputed full signed event ID, even for a short request.
        event_id: EventId,
        /// Exact canonical MIME, verified against the image bytes.
        mime: &'static str,
        /// Encoded width before orientation metadata.
        width: u32,
        /// Encoded height before orientation metadata.
        height: u32,
        /// Original encoded bytes; no normalization or metadata stripping.
        data: Vec<u8>,
    },
    /// No retained envelope or currently processed payload.
    Missing,
    /// Author deletion hides the payload, even if deduplicated bytes remain.
    Deleted,
    /// Local retention policy pruned this event's payload.
    Pruned,
    /// Corrupt storage, failed authentication, malformed payload or image.
    Invalid,
    /// Valid historical encodings/formats outside the native subset.
    Unsupported,
    /// Encoded body, media bytes, dimensions or pixel count exceed policy.
    TooLarge,
    /// Retained event does not match the requested full ID, author or kind.
    Mismatch,
}

impl Database {
    /// Read and validate locally retained signed PNG/WebP without fetching.
    ///
    /// Raw DB value and signed body lengths are checked before payload
    /// allocation. Complete CBOR shape is checked before extracting image
    /// bytes. Raster validation reuses the avatar *presentation* policy; it
    /// does not rewrite uploads. It runs in a blocking section, after
    /// releasing the read snapshot. Hosts must bound concurrency and avoid
    /// the UI executor. Bounds are not a CPU deadline or process RSS
    /// ceiling. A concurrent later delete cannot revoke already-returned
    /// bytes; hosts must re-read on redisplay.
    pub async fn read_native_media(
        &self,
        author: RostraId,
        reference: MediaEventReference,
    ) -> DbResult<NativeMediaRead> {
        let result = self
            .read_native_media_checked(author, reference, true)
            .await?;
        Ok(tokio::task::block_in_place(|| validate(result)))
    }

    /// Read authenticated original retained media for explicit opaque export.
    ///
    /// Supply the actual retained source post's author, never an identity
    /// inferred from a caption. This shares every envelope, lifecycle, size,
    /// hash and complete CBOR check with `read_native_media`, but does not
    /// validate raster content or trust its MIME declaration. Available is not
    /// permission to execute, open or display the bytes. No filename, extension
    /// or path is chosen here. No fetch, scheduling, signing or write occurs.
    ///
    /// Hosts must bound concurrency, avoid the UI executor, and re-read on
    /// later use. Availability covers one snapshot: a later deletion cannot
    /// revoke already-returned bytes. There is no CPU deadline or RSS
    /// guarantee. Database failures remain `DbError`, separate from media
    /// availability.
    pub async fn read_native_media_opaque(
        &self,
        author: RostraId,
        reference: MediaEventReference,
    ) -> DbResult<OpaqueMediaRead> {
        self.read_native_media_checked(author, reference, false)
            .await
    }

    async fn read_native_media_checked(
        &self,
        author: RostraId,
        reference: MediaEventReference,
        image_only: bool,
    ) -> DbResult<OpaqueMediaRead> {
        self.read_with(|tx| {
            let key = bincode::encode_to_vec(reference.short_id(), redb_bincode::BINCODE_CONFIG)
                .expect("fixed-size event key");
            let events = tx.open_table(&crate::events::TABLE)?;
            let Some(raw) = events.as_raw().get(key.as_slice())? else {
                return Ok(OpaqueMediaRead::Missing);
            };
            let Some(event) = decode::<EventRecord>(raw.value(), 1024) else {
                return Ok(OpaqueMediaRead::Invalid);
            };
            let id = event.signed.compute_id();
            if event.signed.compute_short_id() != reference.short_id() {
                return Ok(OpaqueMediaRead::Invalid);
            }
            if event.author() != author
                || event.kind() != EventKind::SOCIAL_MEDIA
                || matches!(reference, MediaEventReference::Full(expected) if expected != id)
            {
                return Ok(OpaqueMediaRead::Mismatch);
            }
            if VerifiedEvent::verify_signed(author, event.signed).is_err() {
                return Ok(OpaqueMediaRead::Invalid);
            }
            let states = tx.open_table(&crate::events_content_state::TABLE)?;
            if let Some(raw) = states.as_raw().get(key.as_slice())? {
                use crate::event::EventContentState;
                return Ok(match decode(raw.value(), 64) {
                    Some(EventContentState::Deleted { .. }) => OpaqueMediaRead::Deleted,
                    Some(EventContentState::Pruned) => OpaqueMediaRead::Pruned,
                    Some(EventContentState::Missing { .. }) => OpaqueMediaRead::Missing,
                    Some(EventContentState::Invalid) | None => OpaqueMediaRead::Invalid,
                });
            }
            if event.content_len() as usize > MAX_NATIVE_MEDIA_BODY_BYTES {
                return Ok(OpaqueMediaRead::TooLarge);
            }
            let store = tx.open_table(&crate::content_store::TABLE)?;
            let key = bincode::encode_to_vec(event.content_hash(), redb_bincode::BINCODE_CONFIG)
                .expect("fixed-size content key");
            let Some(raw) = store.as_raw().get(key.as_slice())? else {
                return Ok(OpaqueMediaRead::Missing);
            };
            if raw.value().len() > MAX_NATIVE_MEDIA_BODY_BYTES + 9 {
                return Ok(OpaqueMediaRead::TooLarge);
            }
            // Borrow rather than materializing ContentStoreRecord's owned Vec.
            let Ok((body, consumed)): Result<(&[u8], usize), _> =
                bincode::borrow_decode_from_slice(raw.value(), redb_bincode::BINCODE_CONFIG)
            else {
                return Ok(OpaqueMediaRead::Invalid);
            };
            if consumed != raw.value().len()
                || body.len() != event.content_len() as usize
                || ContentHash::from(blake3::hash(body)) != event.content_hash()
            {
                return Ok(OpaqueMediaRead::Invalid);
            }
            let (mime, data) = match payload::decode(body, image_only) {
                Ok(value) => value,
                Err(issue) => return Ok(issue),
            };
            Ok(OpaqueMediaRead::Available {
                author,
                event_id: id,
                mime: mime.to_owned(),
                data,
            })
        })
        .await
    }
}

fn decode<T: bincode::Decode<()>>(bytes: &[u8], cap: usize) -> Option<T> {
    if bytes.len() > cap {
        return None;
    }
    let (value, consumed) = bincode::decode_from_slice(bytes, redb_bincode::BINCODE_CONFIG).ok()?;
    (consumed == bytes.len()).then_some(value)
}

fn validate(result: OpaqueMediaRead) -> NativeMediaRead {
    let (author, event_id, mime, data) = match result {
        OpaqueMediaRead::Available {
            author,
            event_id,
            mime,
            data,
        } => (author, event_id, mime, data),
        OpaqueMediaRead::Missing => return NativeMediaRead::Missing,
        OpaqueMediaRead::Deleted => return NativeMediaRead::Deleted,
        OpaqueMediaRead::Pruned => return NativeMediaRead::Pruned,
        OpaqueMediaRead::Invalid => return NativeMediaRead::Invalid,
        OpaqueMediaRead::Unsupported => return NativeMediaRead::Unsupported,
        OpaqueMediaRead::TooLarge => return NativeMediaRead::TooLarge,
        OpaqueMediaRead::Mismatch => return NativeMediaRead::Mismatch,
    };
    match validate_raster_avatar(&mime, &data) {
        Ok(info) => NativeMediaRead::Available {
            author,
            event_id,
            mime: info.mime_type,
            width: info.width,
            height: info.height,
            data,
        },
        Err(RasterAvatarError::TooLarge | RasterAvatarError::DimensionsExceeded) => {
            NativeMediaRead::TooLarge
        }
        Err(RasterAvatarError::UnsupportedMime | RasterAvatarError::Animated) => {
            NativeMediaRead::Unsupported
        }
        Err(RasterAvatarError::MimeMismatch | RasterAvatarError::InvalidImage) => {
            NativeMediaRead::Invalid
        }
    }
}
