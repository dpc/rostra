use rostra_core::event::EventExt as _;
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};
use tracing::{debug, warn};

use crate::tables::event::EventContentState;
use crate::{Database, LOG_TARGET, events, events_content_state, tables};

/// Result of peeking at the next missing content entry.
#[derive(Debug, Clone)]
pub struct NextMissingContent {
    /// Scheduled time for the next fetch attempt.
    pub scheduled_time: Timestamp,
    /// The author of the event.
    pub author: RostraId,
    /// The event whose content is missing.
    pub event_id: ShortEventId,
    /// How many fetch attempts have been made so far.
    pub fetch_attempt_count: u16,
}

impl Database {
    /// Check if an event's content is in the missing state.
    pub async fn is_event_content_missing(&self, event_id: ShortEventId) -> bool {
        self.read_with(|tx| {
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            Ok(matches!(
                events_content_state_table
                    .get(&event_id)?
                    .map(|g| g.value()),
                Some(EventContentState::Missing { .. })
            ))
        })
        .await
        .expect("Storage error")
    }

    /// Peek at the next missing content entry (earliest scheduled fetch).
    ///
    /// Transactionally removes inconsistent entries from the front of
    /// `events_content_missing`, then returns the first entry whose timestamp
    /// matches the event's current `Missing` state. Returns `None` if no valid
    /// entry remains.
    pub async fn peek_next_missing_content(&self) -> Option<NextMissingContent> {
        self.write_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let mut events_content_missing_table =
                tx.open_table(&tables::events_content_missing::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            loop {
                let Some((scheduled_time, event_id)) = events_content_missing_table
                    .first()?
                    .map(|first| first.0.value())
                else {
                    return Ok(None);
                };

                let fetch_attempt_count = match events_content_state_table
                    .get(&event_id)?
                    .map(|g| g.value())
                {
                    Some(EventContentState::Missing {
                        fetch_attempt_count,
                        next_fetch_attempt,
                        ..
                    }) if next_fetch_attempt == scheduled_time => fetch_attempt_count,
                    state => {
                        warn!(
                            target: LOG_TARGET,
                            %event_id,
                            %scheduled_time,
                            ?state,
                            "Removing inconsistent content fetch schedule entry"
                        );
                        events_content_missing_table.remove(&(scheduled_time, event_id))?;
                        continue;
                    }
                };

                let Some(event) = events_table.get(&event_id)?.map(|e| e.value()) else {
                    warn!(
                        target: LOG_TARGET,
                        %event_id,
                        %scheduled_time,
                        "Removing content fetch schedule entry without an event"
                    );
                    events_content_missing_table.remove(&(scheduled_time, event_id))?;
                    continue;
                };

                return Ok(Some(NextMissingContent {
                    scheduled_time,
                    author: event.signed.author(),
                    event_id,
                    fetch_attempt_count,
                }));
            }
        })
        .await
        .expect("Storage error")
    }

    /// Record a failed content fetch attempt.
    ///
    /// Updates the `events_content_missing` schedule entry and the
    /// `events_content_state` metadata for the given event.
    ///
    /// The caller provides both the factual time of the attempt
    /// (`attempted_at`) and the scheduling decision (`next_attempt_at`).
    /// The backoff calculation lives in the fetcher, not in the DB layer.
    ///
    /// The update is compare-and-set against `old_scheduled_time`. If another
    /// completion or a terminal content transition already changed the current
    /// state, this stale completion has no effect. The replacement schedule
    /// must be strictly later than the observed schedule so a schedule value
    /// cannot be reused as an ABA-prone compare-and-set token.
    pub async fn record_failed_content_fetch(
        &self,
        event_id: ShortEventId,
        old_scheduled_time: Timestamp,
        attempted_at: Timestamp,
        next_attempt_at: Timestamp,
    ) {
        self.write_with(|tx| {
            let mut events_content_missing_table =
                tx.open_table(&tables::events_content_missing::TABLE)?;
            let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            // Read current state
            let old_state = events_content_state_table
                .get(&event_id)?
                .map(|g| g.value());

            let Some(EventContentState::Missing {
                fetch_attempt_count,
                next_fetch_attempt: current_scheduled_time,
                ..
            }) = old_state
            else {
                // Not in Missing state anymore (was processed, deleted, etc.)
                // Nothing to update.
                return Ok(());
            };

            if old_scheduled_time != current_scheduled_time {
                debug!(
                    target: LOG_TARGET,
                    %event_id,
                    %old_scheduled_time,
                    %current_scheduled_time,
                    "Ignoring stale failed content fetch completion"
                );
                return Ok(());
            }

            if next_attempt_at <= current_scheduled_time {
                warn!(
                    target: LOG_TARGET,
                    %event_id,
                    %current_scheduled_time,
                    %next_attempt_at,
                    "Ignoring failed content fetch completion with a non-forward schedule"
                );
                return Ok(());
            }

            // Remove the schedule entry mirrored by the current state.
            events_content_missing_table.remove(&(current_scheduled_time, event_id))?;

            // Insert new schedule entry with updated time
            events_content_missing_table.insert(&(next_attempt_at, event_id), &())?;

            // Update state with new attempt metadata
            let new_count = fetch_attempt_count.saturating_add(1);
            events_content_state_table.insert(
                &event_id,
                &EventContentState::Missing {
                    last_fetch_attempt: Some(attempted_at),
                    fetch_attempt_count: new_count,
                    next_fetch_attempt: next_attempt_at,
                },
            )?;

            Ok(())
        })
        .await
        .expect("Storage error")
    }

    /// Paginate through missing content entries.
    ///
    /// Returns events sorted by their next scheduled fetch time. Inconsistent
    /// rows are omitted; fetcher peeking performs their transactional removal.
    pub async fn paginate_missing_events_contents(
        &self,
        cursor: Option<(Timestamp, ShortEventId)>,
        limit: usize,
    ) -> (
        Vec<(RostraId, ShortEventId)>,
        Option<(Timestamp, ShortEventId)>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_content_missing_table =
                tx.open_table(&tables::events_content_missing::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            Self::paginate_table(
                &events_content_missing_table,
                cursor,
                limit,
                move |(scheduled_time, event_id), _| {
                    if !matches!(
                        events_content_state_table
                            .get(&event_id)?
                            .map(|state| state.value()),
                        Some(EventContentState::Missing {
                            next_fetch_attempt,
                            ..
                        }) if next_fetch_attempt == scheduled_time
                    ) {
                        return Ok(None);
                    }

                    let Some(event) = events_table.get(&event_id)?.map(|e| e.value()) else {
                        warn!(
                            target: LOG_TARGET,
                            %event_id,
                            "Missing event for content_missing event?!"
                        );
                        return Ok(None);
                    };

                    Ok(Some((event.signed.author(), event_id)))
                },
            )
        })
        .await
        .expect("Storage error")
    }
}
