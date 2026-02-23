use convi::CastFrom;
use ed25519_dalek::SignatureError;
use snafu::{ResultExt as _, Snafu};

use super::{Event, EventContentRaw, EventExt, EventSignature, SignedEvent, SignedEventExt};
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
#[derive(Copy, Clone, Debug)]
pub struct VerifiedEvent {
    pub event_id: EventId,
    pub event: Event,
    pub sig: EventSignature,
}

impl EventExt for VerifiedEvent {
    fn event(&self) -> &Event {
        &self.event
    }
}

impl SignedEventExt for VerifiedEvent {
    fn sig(&self) -> EventSignature {
        self.sig
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedEventContent {
    pub event: VerifiedEvent,
    pub content: Option<EventContentRaw>,
}

impl EventExt for VerifiedEventContent {
    fn event(&self) -> &Event {
        &self.event.event
    }
}

impl VerifiedEventContent {
    pub fn event_id(&self) -> EventId {
        self.event.event_id
    }
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
    /// Verify event that was asked for by `(author, event_id)`
    pub fn verify_response(
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

        event.verify_signature(sig).context(SignatureInvalidSnafu)?;

        Ok(Self {
            event_id,
            event,
            sig,
        })
    }

    /// Verify event received event
    pub fn verify_received_as_is(
        SignedEvent { event, sig }: SignedEvent,
    ) -> VerifiedEventResult<Self> {
        event.verify_signature(sig).context(SignatureInvalidSnafu)?;

        Ok(Self {
            event_id: event.compute_id(),
            event,
            sig,
        })
    }

    pub fn assume_verified_from_signed(SignedEvent { event, sig }: SignedEvent) -> Self {
        debug_assert!(
            VerifiedEvent::verify_received_as_is(SignedEvent::unverified(event, sig)).is_ok()
        );
        Self {
            event_id: event.compute_id(),
            event,
            sig,
        }
    }

    pub fn verify_signed(
        author: RostraId,
        SignedEvent { event, sig }: SignedEvent,
    ) -> VerifiedEventResult<Self> {
        Self::verify_response(author, event.compute_id(), event, sig)
    }
}

impl VerifiedEventContent {
    pub fn verify(
        event: VerifiedEvent,
        content: impl Into<Option<EventContentRaw>>,
    ) -> VerifiedEventResult<Self> {
        let content = content.into();
        if let Some(content) = content.as_ref() {
            if content.len() != usize::cast_from(event.content_len()) {
                return ContentMismatchSnafu.fail();
            }
            if content.compute_content_hash() != event.content_hash() {
                return ContentMismatchSnafu.fail();
            }
        } else {
            // No content provided — only valid for events with content_len == 0.
            if event.content_len() != 0 {
                return ContentMismatchSnafu.fail();
            }
        }

        Ok(Self { event, content })
    }
    pub fn assume_verified(
        event: VerifiedEvent,
        content: impl Into<Option<EventContentRaw>>,
    ) -> Self {
        let content = content.into();
        debug_assert!(VerifiedEventContent::verify(event, content.clone()).is_ok());
        Self { event, content }
    }
}
