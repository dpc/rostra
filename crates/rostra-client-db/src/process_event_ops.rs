use rostra_core::Timestamp;
use rostra_core::event::{EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::ToShort as _;
use rostra_util_error::FmtCompact as _;
use tracing::{debug, info, warn};

use crate::event::ContentStoreRecord;
use crate::process_event_content_ops::ProcessEventError;
use crate::{
    Database, DbResult, EventReceivedRecord, EventReceivedSource, InsertEventOutcome, LOG_TARGET,
    ProcessEventState, WriteTransactionCtx, content_rc, content_store, events, events_by_time,
    events_content_missing, events_content_state, events_heads, events_missing, events_received_at,
    ids_data_usage, ids_full,
};

impl Database {
    /// Process a received event, inserting it into the database.
    ///
    /// The `now` parameter should be `Timestamp::now()` for normal operation,
    /// but can be set to a specific value for testing or migration.
    pub fn process_event_tx(
        &self,
        event: &VerifiedEvent,
        now: Timestamp,
        tx: &WriteTransactionCtx,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let mut events_tbl = tx.open_table(&events::TABLE)?;
        let mut events_content_state_tbl = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_tbl = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_tbl = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_tbl = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_missing_tbl = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_tbl = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_tbl = tx.open_table(&events_by_time::TABLE)?;
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut ids_data_usage_tbl = tx.open_table(&ids_data_usage::TABLE)?;

        let insert_event_outcome = Database::insert_event_tx(
            *event,
            &mut ids_full_tbl,
            &mut events_tbl,
            &mut events_missing_tbl,
            &mut events_heads_tbl,
            &mut events_by_time_tbl,
            &mut events_content_state_tbl,
            &mut content_store_tbl,
            &mut content_rc_tbl,
            &mut events_content_missing_tbl,
            Some(&mut ids_data_usage_tbl),
        )?;

        if let InsertEventOutcome::Inserted {
            was_missing,
            is_deleted,
            deleted_parent,
            ref missing_parents,
            ref reverted_parent_content,
            ..
        } = insert_event_outcome
        {
            // Record when we received this event
            let mut events_received_at_tbl = tx.open_table(&events_received_at::TABLE)?;
            Self::insert_reception_ordered_tx(
                tx,
                now,
                &EventReceivedRecord {
                    event_id: event.event_id.to_short(),
                    source: EventReceivedSource::Pushed {
                        from_id: None,
                        from_node: None,
                    },
                },
                &mut events_received_at_tbl,
            )?;

            if is_deleted {
                info!(target: LOG_TARGET,
                    event_id = %event.event_id,
                    author = %event.event.author,
                    parent_prev = %event.event.parent_prev,
                    parent_aux = %event.event.parent_aux,
                    "Event content was already deleted; header effects applied"
                );
                if let Some(ContentStoreRecord(content)) = content_store_tbl
                    .get(&event.content_hash())?
                    .map(|record| record.value())
                {
                    match VerifiedEventContent::verify(*event, content.into_owned()) {
                        Ok(event_content) => {
                            match Self::process_deleted_social_post_replacement_tx(
                                &event_content,
                                tx,
                            ) {
                                Ok(_) => {}
                                Err(ProcessEventError::Db { source }) => return Err(source),
                                Err(ProcessEventError::Invalid { source, location }) => {
                                    debug!(
                                        target: LOG_TARGET,
                                        err = %source.as_ref().fmt_compact(),
                                        %location,
                                        event_id = %event.event_id,
                                        author = %event.author(),
                                        "Ignoring malformed retained Deleted social-post content"
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            warn!(
                                target: LOG_TARGET,
                                ?err,
                                event_id = %event.event_id,
                                "Ignoring retained content that does not verify against its event"
                            );
                        }
                    }
                }
            } else {
                info!(target: LOG_TARGET,
                    kind = %event.kind(),
                    event_id = %event.event_id.to_short(),
                    author = %event.event.author.to_short(),
                    parent_prev = %event.event.parent_prev,
                    parent_aux = %event.event.parent_aux,
                    "New event inserted"
                );

                // Not missing, means it's a head event (no known children yet)
                // Broadcast new head event to subscribers
                if !was_missing {
                    let sender = self.new_heads_tx.clone();
                    let author = event.author();
                    let event_id = event.event_id.into();
                    tx.on_commit(move || {
                        let _ = sender.send((author, event_id));
                    });
                }
            }

            if event.event.author == self.self_id {
                let mut events_self_table = tx.open_table(&crate::events_self::TABLE)?;
                Database::insert_self_event_id_tx(event.event_id, &mut events_self_table)?;

                if !was_missing {
                    info!(target: LOG_TARGET, event_id = %event.event_id, "New self head");
                }

                let sender = self.self_head_updated.clone();
                let self_head = Database::read_head_tx(self.self_id, &events_heads_tbl)?;
                tx.on_commit(move || {
                    sender.send_replace(self_head);
                });
            }

            if !missing_parents.is_empty() {
                let mut missing_event_tx = self.ids_with_missing_events_tx.clone();
                let author = event.author();
                tx.on_commit(move || {
                    missing_event_tx.send(author);
                })
            }

            // if the event reverted any previously processed content, revert it here
            if let Some(reverted_content) = reverted_parent_content {
                let event_id = deleted_parent.expect("Must have the deleted event id");
                let event = events_tbl
                    .get(&event_id)?
                    .expect("Must have the event")
                    .value();
                let verified_event = VerifiedEvent::assume_verified_from_signed(event.signed);
                let verified_event_content =
                    VerifiedEventContent::assume_verified(verified_event, reverted_content.clone());
                match self.process_event_content_reverted_tx(&verified_event_content, tx) {
                    Ok(()) => {}
                    Err(ProcessEventError::Db { source }) => return Err(source),
                    Err(ProcessEventError::Invalid { source, location }) => {
                        warn!(
                            target: LOG_TARGET,
                            err = %source.as_ref().fmt_compact(),
                            %location,
                            "Could not process reverting a previous valid content?! Ignoring, but a sign of a bug."
                        );
                    }
                };
            }
        }

        let process_event_content_state =
            if Self::MAX_CONTENT_LEN < u32::from(event.event.content_len) {
                if Database::prune_event_content_tx(
                    event.event_id,
                    event.content_hash(),
                    &mut events_content_state_tbl,
                    &mut content_rc_tbl,
                    &mut events_content_missing_tbl,
                    Some((event.author(), event.content_len(), &mut ids_data_usage_tbl)),
                )? {
                    ProcessEventState::Pruned
                } else {
                    ProcessEventState::Deleted
                }
            } else {
                match insert_event_outcome {
                    InsertEventOutcome::AlreadyPresent => ProcessEventState::Existing,
                    InsertEventOutcome::Inserted { is_deleted, .. } => {
                        if is_deleted {
                            ProcessEventState::Deleted
                        } else {
                            // If the event was not there, and it wasn't deleted
                            // it definitely does not have content yet.
                            ProcessEventState::New
                        }
                    }
                }
            };

        // Notify the content fetcher when new content needs fetching.
        // This fires when a new event with content_len > 0 was inserted and
        // its content is not already available (ProcessEventState::New).
        if matches!(process_event_content_state, ProcessEventState::New) && 0 < event.content_len()
        {
            let notify = self.content_missing_notify.clone();
            tx.on_commit(move || {
                notify.notify_one();
            });
        }

        Ok((insert_event_outcome, process_event_content_state))
    }
}
