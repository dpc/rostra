//! Exact-full-identity, bounded local public profile projection reads.
//!
//! This is not graph membership, remote discovery, or signed-body verification.
//! Names, MIME and bytes are retained projection data. In particular, an avatar
//! is not validated as a safe image: callers must apply their own bounded
//! decoder policy before display. No bio is copied, no pixels decoded, and no
//! I/O other than local database reads is performed.

use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::{EventId, ShortEventId, Timestamp};

use crate::known_people::{NAME_BYTES_MAX, PROFILE_BYTES_MAX, ProfileField, ProfileIssue};
use crate::{Database, DbResult, EventRecord};

#[cfg(test)]
mod tests;

/// Maximum owned avatar payload, independent of the raw profile cap.
pub const AVATAR_BYTES_MAX: usize = 1_000_000;
/// Maximum owned declared MIME string in UTF-8 bytes.
pub const MIME_BYTES_MAX: usize = 64;

/// Untrusted encoded avatar bytes and their unvalidated declared MIME.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProfileAvatar {
    /// Original declaration, not a verified format or safe decoder selection.
    pub declared_mime: String,
    /// Original retained encoded bytes, never normalized or pixel-decoded.
    pub data: Vec<u8>,
}

/// Bounded fields from one local projection and its matching source envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProfile {
    /// Exact requested full author identity.
    pub identity: RostraId,
    /// Recomputed full identity of the retained verified source envelope.
    pub event_id: EventId,
    /// Timestamp matching that source envelope.
    pub timestamp: Timestamp,
    /// BLAKE3 digest of the complete bounded raw projection.
    ///
    /// Cache by account/database context, identity, event ID and revision.
    /// Re-read on later presentation; neither this digest nor an event ID is
    /// enduring availability authority. Replacements/removal/unavailable reads
    /// must replace cached state, never retain a previous avatar as current.
    /// A digest change includes same-event projection repairs and bio changes.
    pub revision: [u8; 32],
    /// At most 1024 UTF-8 bytes; unavailable independently of avatar size.
    pub display_name: ProfileField<String>,
    /// None means the projection explicitly has no avatar, not a read failure.
    pub avatar: ProfileField<Option<LocalProfileAvatar>>,
}

/// Snapshot availability, separate from identity existence or graph membership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalProfileRead {
    /// A shape-valid bounded projection with a matching retained source.
    Available(LocalProfile),
    /// No usable projection; the reason carries no copied profile data.
    Unavailable(ProfileIssue),
}

type BorrowedProfile<'a> = (
    Timestamp,
    ShortEventId,
    &'a str,
    &'a str,
    Option<(&'a str, &'a [u8])>,
);

impl Database {
    /// Read one exact full identity's retained public profile, without
    /// traversal.
    ///
    /// The raw profile's 2 MiB cap is checked before decoding, hashing or
    /// copying any profile field. Complete borrowed shape validation
    /// precedes allocation. The source envelope has a separate 1024-byte
    /// raw cap and must match the full author, kind, timestamp, short event
    /// ID and signature. This does not authenticate the projection against
    /// the source body or fetch that body.
    ///
    /// Name, MIME and avatar copies are independently bounded (1024, 64 and
    /// 1,000,000 bytes); oversized fields are explicit, not truncated. Database
    /// failures remain outer errors. Run off the UI executor and bound
    /// concurrent calls: these limits are not a CPU deadline or process RSS
    /// guarantee.
    pub async fn read_local_profile(&self, identity: RostraId) -> DbResult<LocalProfileRead> {
        self.read_with(|tx| {
            let profiles = tx.open_table(&crate::social_profiles::TABLE)?;
            let key = bincode::encode_to_vec(identity, redb_bincode::BINCODE_CONFIG)
                .map_err(|error| -> rostra_util_error::BoxedError { Box::new(error) })?;
            let Some(raw) = profiles.as_raw().get(key.as_slice())? else {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::Missing));
            };
            let raw = raw.value();
            if raw.len() > PROFILE_BYTES_MAX {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::Oversized));
            }
            let Ok(((timestamp, short_id, name, _bio, avatar), consumed)) =
                bincode::borrow_decode_from_slice::<BorrowedProfile<'_>, _>(
                    raw,
                    redb_bincode::BINCODE_CONFIG,
                )
            else {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::Corrupt));
            };
            if consumed != raw.len() {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::Corrupt));
            }
            let events = tx.open_table(&crate::events::TABLE)?;
            let key = bincode::encode_to_vec(short_id, redb_bincode::BINCODE_CONFIG)
                .map_err(|error| -> rostra_util_error::BoxedError { Box::new(error) })?;
            let Some(event_raw) = events.as_raw().get(key.as_slice())? else {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::MissingSource));
            };
            let event_raw = event_raw.value();
            if event_raw.len() > 1024 {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::Corrupt));
            }
            let Ok((event, consumed)): Result<(EventRecord, usize), _> =
                bincode::decode_from_slice(event_raw, redb_bincode::BINCODE_CONFIG)
            else {
                return Ok(LocalProfileRead::Unavailable(ProfileIssue::Corrupt));
            };
            let event_id = event.signed.compute_id();
            if consumed != event_raw.len()
                || event.author() != identity
                || event.kind() != EventKind::SOCIAL_PROFILE_UPDATE
                || event.timestamp() != timestamp
                || event.signed.compute_short_id() != short_id
                || VerifiedEvent::verify_signed(identity, event.signed).is_err()
            {
                return Ok(LocalProfileRead::Unavailable(
                    ProfileIssue::MismatchedSource,
                ));
            }
            let display_name = if name.len() > NAME_BYTES_MAX {
                ProfileField::Unavailable(ProfileIssue::Oversized)
            } else {
                ProfileField::Available(name.to_owned())
            };
            let avatar = match avatar {
                Some((mime, data))
                    if mime.len() > MIME_BYTES_MAX || data.len() > AVATAR_BYTES_MAX =>
                {
                    ProfileField::Unavailable(ProfileIssue::Oversized)
                }
                avatar => ProfileField::Available(avatar.map(|(mime, data)| LocalProfileAvatar {
                    declared_mime: mime.to_owned(),
                    data: data.to_vec(),
                })),
            };
            Ok(LocalProfileRead::Available(LocalProfile {
                identity,
                event_id,
                timestamp,
                revision: *blake3::hash(raw).as_bytes(),
                display_name,
                avatar,
            }))
        })
        .await
    }
}
