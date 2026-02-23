use rostra_core::event::EventExt as _;
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};
use tracing::warn;

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
    /// Returns the entry with the smallest `(Timestamp, ShortEventId)` key
    /// from `events_content_missing`, which is the next entry due for
    /// fetching. Returns `None` if the table is empty.
    pub async fn peek_next_missing_content(&self) -> Option<NextMissingContent> {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_content_missing_table =
                tx.open_table(&tables::events_content_missing::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            let Some(first) = events_content_missing_table.first()? else {
                return Ok(None);
            };

            let (scheduled_time, event_id) = first.0.value();

            let Some(event) = events_table.get(&event_id)?.map(|e| e.value()) else {
                warn!(
                    target: LOG_TARGET,
                    %event_id,
                    "Missing event record for content_missing entry"
                );
                return Ok(None);
            };

            let fetch_attempt_count = match events_content_state_table
                .get(&event_id)?
                .map(|g| g.value())
            {
                Some(EventContentState::Missing {
                    fetch_attempt_count,
                    ..
                }) => fetch_attempt_count,
                _ => 0,
            };

            Ok(Some(NextMissingContent {
                scheduled_time,
                author: event.signed.author(),
                event_id,
                fetch_attempt_count,
            }))
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
                ..
            }) = old_state
            else {
                // Not in Missing state anymore (was processed, deleted, etc.)
                // Nothing to update.
                return Ok(());
            };

            // Remove old schedule entry
            events_content_missing_table.remove(&(old_scheduled_time, event_id))?;

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
    /// Returns events sorted by their next scheduled fetch time.
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

            Self::paginate_table(
                &events_content_missing_table,
                cursor,
                limit,
                move |(_, event_id), _| {
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
