use convi::CastFrom;
use ed25519_dalek::SignatureError;
use snafu::{ResultExt as _, Snafu};

use super::{Event, EventContent, EventSignature, SignedEvent};
use crate::EventId;

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
    pub content: Option<EventContent>,
}

#[derive(Debug, Snafu)]
pub enum VerifiedEventError {
    SignatureInvalid { source: SignatureError },
    ContentMismatch,
    EventIdMismatch,
}

pub type VerifiedEventResult<T> = Result<T, VerifiedEventError>;

impl VerifiedEvent {
    pub fn verify(
        event_id: EventId,
        event: Event,
        sig: EventSignature,
        content: impl Into<Option<EventContent>>,
    ) -> VerifiedEventResult<Self> {
        if event.compute_id() != event_id {
            return EventIdMismatchSnafu.fail();
        }

        event
            .verified_signed_by(sig, event.author)
            .context(SignatureInvalidSnafu)?;

        let content: Option<_> = content.into();

        if let Some(content) = content.as_ref() {
            if content.len() != usize::cast_from(u32::from(event.content_len)) {
                return ContentMismatchSnafu.fail();
            }
            if content.compute_content_hash() != event.content_hash {
                return ContentMismatchSnafu.fail();
            }
        }

        Ok(Self {
            event_id,
            event,
            sig,
            content,
        })
    }

    pub fn verify_signed(
        SignedEvent { event, sig }: SignedEvent,
        content: impl Into<Option<EventContent>>,
    ) -> VerifiedEventResult<Self> {
        Self::verify(event.compute_id(), event, sig, content)
    }
}
