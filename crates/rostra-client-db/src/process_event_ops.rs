use rostra_core::ContentHash;
use rostra_core::event::{EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::ToShort as _;
use rostra_util_error::FmtCompact as _;
use tracing::{info, warn};

use crate::process_event_content_ops::ProcessEventError;
use crate::{
    Database, DbResult, InsertEventOutcome, LOG_TARGET, ProcessEventState, WriteTransactionCtx,
    events, events_by_time, events_content, events_content_missing, events_content_rc_count,
    events_heads, events_missing, ids_full,
};

impl Database {
    pub fn process_event_tx(
        &self,
        event: &VerifiedEvent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let mut events_tbl = tx.open_table(&events::TABLE)?;
        let mut events_content_tbl = tx.open_table(&events_content::TABLE)?;
        let mut events_content_rc_count_tbl = tx.open_table(&events_content_rc_count::TABLE)?;
        let mut events_content_missing_tbl = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_missing_tbl = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_tbl = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_tbl = tx.open_table(&events_by_time::TABLE)?;
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;

        let insert_event_outcome = Database::insert_event_tx(
            *event,
            &mut ids_full_tbl,
            &mut events_tbl,
            &mut events_missing_tbl,
            &mut events_heads_tbl,
            &mut events_by_time_tbl,
            &mut events_content_tbl,
            &mut events_content_rc_count_tbl,
            &mut events_content_missing_tbl,
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
            if is_deleted {
                info!(target: LOG_TARGET,
                    event_id = %event.event_id,
                    author = %event.event.author,
                    parent_prev = %event.event.parent_prev,
                    parent_aux = %event.event.parent_aux,
                    "Ignoring already deleted event"
                );
            } else {
                info!(target: LOG_TARGET,
                    kind = %event.kind(),
                    event_id = %event.event_id.to_short(),
                    author = %event.event.author.to_short(),
                    parent_prev = %event.event.parent_prev,
                    parent_aux = %event.event.parent_aux,
                    "New event inserted"
                );
                if event.event.author == self.self_id {
                    let mut events_self_table = tx.open_table(&crate::events_self::TABLE)?;
                    Database::insert_self_event_id_tx(event.event_id, &mut events_self_table)?;

                    if !was_missing {
                        info!(target: LOG_TARGET, event_id = %event.event_id, "New self head");

                        let sender = self.self_head_updated.clone();
                        let event_id = event.event_id.into();
                        tx.on_commit(move || {
                            let _ = sender.send(Some(event_id));
                        });
                    }
                }
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

        let process_event_content_state = if event.event.content_hash() == ContentHash::ZERO {
            ProcessEventState::NoContent
        } else if Self::MAX_CONTENT_LEN < u32::from(event.event.content_len) {
            if Database::prune_event_content_tx(
                event.event_id,
                &mut events_content_tbl,
                &mut events_content_missing_tbl,
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

        if process_event_content_state == ProcessEventState::New {
            events_content_missing_tbl.insert(&event.event_id.to_short(), &())?;
        }
        Ok((insert_event_outcome, process_event_content_state))
    }
}
