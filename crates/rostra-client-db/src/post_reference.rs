//! Bounded, local-only post routing evidence from one retained DB snapshot.
//!
//! No payload, replacement lineage, table scan, network request, or write is
//! performed. The result describes the requested event, not its latest display
//! version. Only after full-ID validation may a host pass the requested short
//! ID to an existing conversation reader that canonicalizes replacement
//! lineages.

use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, ShortRostraId};
use rostra_core::post::{PostAuthorReference, PostEventReference, PostReference};
use rostra_core::{EventId, ShortEventId};

use crate::{Database, DbResult, EventContentAvailability, EventContentState, EventRecord};

#[cfg(test)]
mod tests;

/// Retained evidence for a syntactically valid post reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostReferenceResolution {
    /// The short author has no retained identity-index entry; remain inert.
    UnknownAuthor,
    /// No envelope is retained. Only a short event may become an explicit
    /// unavailable-conversation target; an unknown full event must remain
    /// inert.
    UnknownEvent {
        /// Full author, supplied explicitly or resolved through the index.
        author: RostraId,
        /// Unmodified requested component, never guessed from a prefix.
        event: PostEventReference,
    },
    /// The retained prefix does not match the complete requested event hash.
    FullEventMismatch,
    /// The authenticated event belongs to a different author.
    AuthorMismatch,
    /// The authenticated event is not a social post.
    UnsupportedKind,
    /// Corrupt retained bytes or an unauthenticated signed envelope.
    Invalid,
    /// Valid requested routing evidence, not a claim of supported display
    /// content or availability of a later replacement.
    Resolved {
        /// Authenticated author, equal to the claimed full/resolved author.
        author: RostraId,
        /// Exact requested envelope ID; safe to shorten for conversation
        /// lookup.
        requested_event_id: EventId,
        /// Requested event's durable lifecycle, without reading its payload.
        availability: EventContentAvailability,
    },
}

impl Database {
    /// Resolve exact post components using constant-count bounded point reads.
    ///
    /// Re-resolve on use; this snapshot is not permanent authority or a cache
    /// capability. Hosts must bound concurrency and run off their UI executor.
    /// Storage I/O failures remain errors, not unknown-reference outcomes.
    pub async fn resolve_post_reference(
        &self,
        reference: PostReference,
    ) -> DbResult<PostReferenceResolution> {
        self.read_with(|tx| {
            use PostReferenceResolution as Resolution;
            let author = match reference.author {
                PostAuthorReference::Full(author) => author,
                PostAuthorReference::Short(prefix) => {
                    let Some(author) = crate::ids_full::get_checked(tx, prefix)? else {
                        return Ok(Resolution::UnknownAuthor);
                    };
                    author
                }
            };
            let short: ShortEventId = match reference.event {
                PostEventReference::Full(id) => id.into(),
                PostEventReference::Short(id) => id,
            };
            let key = bincode::encode_to_vec(short, redb_bincode::BINCODE_CONFIG)
                .expect("fixed-size event key");
            let events = tx.open_table(&crate::events::TABLE)?;
            let Some(raw) = events.as_raw().get(key.as_slice())? else {
                return Ok(Resolution::UnknownEvent {
                    author,
                    event: reference.event,
                });
            };
            let Some(event) = decode::<EventRecord>(raw.value(), 1024) else {
                return Ok(Resolution::Invalid);
            };
            let full = event.signed.compute_id();
            if ShortEventId::from(full) != short
                || VerifiedEvent::verify_signed(event.author(), event.signed).is_err()
            {
                return Ok(Resolution::Invalid);
            }
            if matches!(reference.event, PostEventReference::Full(id) if id != full) {
                return Ok(Resolution::FullEventMismatch);
            }
            if event.author() != author {
                return Ok(Resolution::AuthorMismatch);
            }
            if event.kind() != EventKind::SOCIAL_POST {
                return Ok(Resolution::UnsupportedKind);
            }
            let states = tx.open_table(&crate::events_content_state::TABLE)?;
            let availability = match states.as_raw().get(key.as_slice())? {
                None => EventContentAvailability::Available,
                Some(raw) => match decode::<EventContentState>(raw.value(), 64) {
                    Some(EventContentState::Missing { .. }) => EventContentAvailability::Missing,
                    Some(EventContentState::Deleted { .. }) => EventContentAvailability::Deleted,
                    Some(EventContentState::Invalid) => EventContentAvailability::Invalid,
                    Some(EventContentState::Pruned) => {
                        let decisions = tx.open_table(&crate::events_quota_pruned::TABLE)?;
                        let reason = match decisions.as_raw().get(key.as_slice())? {
                            None => None,
                            Some(raw) => {
                                let Some(decision) = decode::<crate::retention::QuotaPruneDecision>(
                                    raw.value(),
                                    128,
                                ) else {
                                    return Ok(Resolution::Invalid);
                                };
                                Some(decision.reason)
                            }
                        };
                        EventContentAvailability::Pruned {
                            quota_reason: reason,
                        }
                    }
                    None => return Ok(Resolution::Invalid),
                },
            };
            Ok(Resolution::Resolved {
                author,
                requested_event_id: full,
                availability,
            })
        })
        .await
    }

    /// Fallibly resolve a canonical short author through the
    /// collision-protected retained index, without scanning identities or
    /// discovering peers.
    pub async fn try_get_known_identity(
        &self,
        prefix: ShortRostraId,
    ) -> DbResult<Option<RostraId>> {
        self.read_with(|tx| crate::ids_full::get_checked(tx, prefix))
            .await
    }
}

fn decode<T: bincode::Decode<()>>(bytes: &[u8], limit: usize) -> Option<T> {
    if bytes.len() > limit {
        return None;
    }
    let (value, used) = bincode::decode_from_slice(bytes, redb_bincode::BINCODE_CONFIG).ok()?;
    (used == bytes.len()).then_some(value)
}
