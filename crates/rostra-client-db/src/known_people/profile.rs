//! Borrowed bincode projection inspection, never cloning bios or avatars.

use redb_bincode::ReadTransaction;
use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};

use super::{NAME_BYTES_MAX, PROFILE_BYTES_MAX, PersonProfile, ProfileField, ProfileIssue, read};
use crate::{DbResult, EventRecord};

fn unavailable(issue: ProfileIssue) -> PersonProfile {
    PersonProfile {
        display_name: ProfileField::Unavailable(issue),
        timestamp: ProfileField::Unavailable(issue),
    }
}

type BorrowedProfile<'a> = (
    Timestamp,
    ShortEventId,
    &'a str,
    &'a str,
    Option<(&'a str, &'a [u8])>,
);

pub(super) fn read(tx: &ReadTransaction, identity: RostraId) -> DbResult<PersonProfile> {
    let profiles = tx.open_table(&crate::social_profiles::TABLE)?;
    let Some(raw) = profiles.as_raw().get(read::encoded(identity)?.as_slice())? else {
        return Ok(unavailable(ProfileIssue::Missing));
    };
    let raw = raw.value();
    // Header-only decoding also permits independent timestamp availability when
    // the profile tail is oversized/corrupt. Both header fields are fixed-size
    // values with at most 25 encoded bytes; no length-controlled allocation.
    let Ok(((timestamp, event_id), _)): Result<((Timestamp, ShortEventId), usize), _> =
        bincode::decode_from_slice(&raw[..raw.len().min(32)], redb_bincode::BINCODE_CONFIG)
    else {
        return Ok(unavailable(ProfileIssue::Corrupt));
    };
    let events = tx.open_table(&crate::events::TABLE)?;
    let Some(event_raw) = events.as_raw().get(read::encoded(event_id)?.as_slice())? else {
        return Ok(unavailable(ProfileIssue::MissingSource));
    };
    let event_raw = event_raw.value();
    if event_raw.len() > 1024 {
        return Ok(unavailable(ProfileIssue::Corrupt));
    }
    let Ok((event, consumed)): Result<(EventRecord, usize), _> =
        bincode::decode_from_slice(event_raw, redb_bincode::BINCODE_CONFIG)
    else {
        return Ok(unavailable(ProfileIssue::Corrupt));
    };
    if consumed != event_raw.len()
        || event.author() != identity
        || event.kind() != EventKind::SOCIAL_PROFILE_UPDATE
        || event.timestamp() != timestamp
        || event.signed.compute_short_id() != event_id
        || VerifiedEvent::verify_signed(identity, event.signed).is_err()
    {
        return Ok(unavailable(ProfileIssue::MismatchedSource));
    }
    let display_name = if raw.len() > PROFILE_BYTES_MAX {
        ProfileField::Unavailable(ProfileIssue::Oversized)
    } else {
        // Mirrors Latest<IdSocialProfileRecord>, including the entire tail and
        // strict consumed length. Borrowed slices preflight hostile lengths
        // without allocation; UTF-8 validation touches at most the raw cap.
        match bincode::borrow_decode_from_slice::<BorrowedProfile<'_>, _>(
            raw,
            redb_bincode::BINCODE_CONFIG,
        ) {
            Ok(((_, _, name, _, _), consumed)) if consumed == raw.len() => {
                if name.len() > NAME_BYTES_MAX {
                    ProfileField::Unavailable(ProfileIssue::Oversized)
                } else {
                    ProfileField::Available(name.to_owned())
                }
            }
            _ => ProfileField::Unavailable(ProfileIssue::Corrupt),
        }
    };
    Ok(PersonProfile {
        display_name,
        timestamp: ProfileField::Available(timestamp),
    })
}
