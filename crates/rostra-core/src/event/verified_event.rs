use convi::CastFrom;
use ed25519_dalek::SignatureError;
use snafu::{ResultExt as _, Snafu};

use super::{Event, EventContent, EventSignature, SignedEvent};
use crate::id::RostraId;
use crate::{EventId, ShortEventId};

/// An event with all the external invariants verified
///
/// Invariants:
///
/// * `event_id` matches
/// * `sig` valid for `event.author`
/// * if `content` is `Some`, matches `event.content_hash` and
///   `event.content_len`
#[derive(Clone, Debug)]
pub struct VerifiedEvent {
    pub event_id: EventId,
    pub event: Event,
    pub sig: EventSignature,
}

#[derive(Clone, Debug)]
pub struct VerifiedEventContent {
    pub event_id: EventId,
    pub event: Event,
    pub sig: EventSignature,
    pub content: EventContent,
}

#[derive(Debug, Snafu)]
pub enum VerifiedEventError {
    AuthorMismatch,
    SignatureInvalid { source: SignatureError },
    ContentMismatch,
    EventIdMismatch,
}

pub type VerifiedEventResult<T> = Result<T, VerifiedEventError>;

impl VerifiedEvent {
    pub fn verify_received(
        author: RostraId,
        event_id: impl Into<ShortEventId>,
        event: Event,
        sig: EventSignature,
    ) -> VerifiedEventResult<Self> {
        if author != event.author {
            return AuthorMismatchSnafu.fail();
        }
        let short_event_id: ShortEventId = event_id.into();
        let event_id = event.compute_id();
        if ShortEventId::from(event_id) != short_event_id {
            return EventIdMismatchSnafu.fail();
        }

        event
            .verified_signed_by(sig, event.author)
            .context(SignatureInvalidSnafu)?;

        Ok(Self {
            event_id,
            event,
            sig,
        })
    }

    pub fn verify_signed(
        author: RostraId,
        SignedEvent { event, sig }: SignedEvent,
    ) -> VerifiedEventResult<Self> {
        Self::verify_received(author, event.compute_id(), event, sig)
    }
}

impl VerifiedEventContent {
    pub fn verify(
        VerifiedEvent {
            event_id,
            event,
            sig,
        }: VerifiedEvent,
        content: EventContent,
    ) -> VerifiedEventResult<Self> {
        if content.len() != usize::cast_from(u32::from(event.content_len)) {
            return ContentMismatchSnafu.fail();
        }
        if content.compute_content_hash() != event.content_hash {
            return ContentMismatchSnafu.fail();
        }

        Ok(Self {
            event_id,
            event,
            sig,
            content,
        })
    }
}
