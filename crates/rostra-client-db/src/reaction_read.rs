//! Shared bounded retained-envelope and payload checks for reaction operations.

use rostra_core::event::{EventExt as _, EventKind, SocialPost};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{EventId, ShortEventId};

use crate::{Database, DbError, DbResult, EventRecord};

/// Shared work budget, including rejected and unavailable candidate rows.
#[derive(Default)]
pub(crate) struct Budget {
    /// Total traversed replacement edges.
    edges: usize,
    /// Total retained encoded payload bytes examined.
    bytes: usize,
}

impl Budget {
    fn edge(&mut self, path_steps: usize) -> DbResult<()> {
        self.edges += 1;
        if path_steps > 1024 || self.edges > 4096 {
            return Err(DbError::ReactionHistoryLimit);
        }
        Ok(())
    }
}

pub(crate) fn bound_event(
    target: (RostraId, EventId),
    events: &impl crate::events::ReadableTable,
) -> DbResult<EventRecord> {
    let event =
        Database::get_event_tx(target.1.to_short(), events)?.ok_or(DbError::ReactionTarget)?;
    if event.event().compute_id() != target.1
        || event.author() != target.0
        || event.kind() != EventKind::SOCIAL_POST
        || event.is_singleton()
    {
        return Err(DbError::ReactionTarget);
    }
    Ok(event)
}

/// Return canonical root-to-current history, rejecting noncanonical input
/// forks.
pub(crate) fn versions(
    author: RostraId,
    requested: ShortEventId,
    events: &impl crate::events::ReadableTable,
    replaces: &impl crate::social_posts_replaces::ReadableTable,
    replaced_by: &impl crate::social_posts_replaced_by::ReadableTable,
    budget: &mut Budget,
) -> DbResult<Vec<ShortEventId>> {
    let mut current = requested;
    let mut steps = 0;
    while let Some(row) = replaces
        .range(&(author, current, ShortEventId::ZERO)..=&(author, current, ShortEventId::MAX))?
        .next()
        .transpose()?
    {
        steps += 1;
        budget.edge(steps)?;
        current = row.0.value().2;
    }
    let mut result = Vec::new();
    loop {
        let event = Database::get_event_tx(current, events)?.ok_or(DbError::ReactionUnavailable)?;
        if event.author() != author
            || event.kind() != EventKind::SOCIAL_POST
            || event.is_singleton()
        {
            return Err(DbError::ReactionTarget);
        }
        result.push(current);
        let Some(row) = replaced_by
            .range(&(author, current, ShortEventId::ZERO)..=&(author, current, ShortEventId::MAX))?
            .next()
            .transpose()?
        else {
            break;
        };
        budget.edge(result.len())?;
        current = row.0.value().2;
    }
    if !result.contains(&requested) {
        return Err(DbError::ReactionConflict);
    }
    Ok(result)
}

pub(crate) fn payload(
    event: &EventRecord,
    states: &impl crate::events_content_state::ReadableTable,
    store: &impl crate::content_store::ReadableTable,
    budget: &mut Budget,
    reaction: bool,
) -> DbResult<SocialPost> {
    if Database::get_event_content_state_tx(event.event().compute_id().to_short(), states)?
        .is_some()
    {
        return Err(DbError::ReactionUnavailable);
    }
    let raw = store
        .get(&event.content_hash())?
        .ok_or(DbError::ReactionUnavailable)?;
    if event.content_len() > 1_000_000
        || budget.bytes.saturating_add(event.content_len() as usize) > 16 * 1024 * 1024
    {
        return Err(DbError::ReactionPayloadLimit);
    }
    let raw = raw.value();
    let bytes = raw.0.as_ref().as_ref();
    budget.bytes = budget.bytes.saturating_add(bytes.len());
    if bytes.len() > 1_000_000 || budget.bytes > 16 * 1024 * 1024 {
        return Err(DbError::ReactionPayloadLimit);
    }
    let post = if reaction {
        // Current reducers intentionally do not project deleting e-only events.
        if event.is_delete_parent_aux_content_set() {
            return Err(DbError::ReactionUnsupported);
        }
        SocialPost::decode_mutable_reaction(bytes)
    } else {
        SocialPost::decode_mutable_text(bytes).filter(|post| {
            post.djot_content
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
    };
    post.ok_or(DbError::ReactionUnsupported)
}
