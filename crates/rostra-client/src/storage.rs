use rostra_core::event::{
    content, EventContent, EventKind, PersonaId, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_util_error::FmtCompact as _;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::db::{
    Database, DbResult, InsertEventOutcome, WriteTransactionCtx, TABLE_EVENTS_BY_TIME,
    TABLE_EVENTS_HEADS, TABLE_EVENTS_MISSING, TABLE_EVENTS_SELF,
};

pub struct Storage {
    db: Database,
    self_followee_list_updated: watch::Sender<()>,
    self_head_updated: watch::Sender<Option<ShortEventId>>,
    self_id: RostraId,
    iroh_secret: iroh::SecretKey,
}

pub const LOG_TARGET: &str = "rostra::storage";

impl Storage {
    const MAX_CONTENT_LEN: u32 = 1_000_000u32;

    pub async fn new(db: Database, self_id: RostraId) -> DbResult<Self> {
        // let self_followees = db
        //     .read_followees(self_id)
        //     .await?
        //     .into_iter()
        //     .map(|(id, _)| id)
        //     .collect();

        let self_head = db.get_head(self_id).await?;
        let iroh_secret = db.iroh_secret().await?;

        let (self_followee_list_updated, _) = watch::channel(());
        let (self_head_updated, _) = watch::channel(self_head);

        Ok(Self {
            self_id,
            iroh_secret,
            db,
            self_followee_list_updated,
            self_head_updated,
        })
    }

    pub fn self_followees_list_subscribe(&self) -> watch::Receiver<()> {
        self.self_followee_list_updated.subscribe()
    }

    pub fn self_head_subscribe(&self) -> watch::Receiver<Option<ShortEventId>> {
        self.self_head_updated.subscribe()
    }

    pub async fn has_event(&self, event_id: impl Into<ShortEventId>) -> bool {
        let event_id = event_id.into();
        self.db
            .read_with(|tx| {
                let events_table = tx
                    .open_table(&crate::db::TABLE_EVENTS)
                    .expect("Storage error");
                Database::has_event_tx(event_id, &events_table)
            })
            .await
            .expect("Database panic")
    }

    pub async fn get_self_followees(&self) -> Vec<(RostraId, PersonaId)> {
        self.db
            .read_with(|tx| {
                let ids_followees_table = tx.open_table(&crate::db::TABLE_ID_FOLLOWEES)?;
                Ok(
                    Database::read_followees_tx(self.self_id, &ids_followees_table)?
                        .into_iter()
                        .map(|(id, record)| (id, record.persona))
                        .collect(),
                )
            })
            .await
            .expect("Database panic")
    }

    pub async fn get_event(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> Option<crate::db::events::EventRecord> {
        let event_id = event_id.into();
        self.db
            .read_with(|tx| {
                let events_table = tx.open_table(&crate::db::TABLE_EVENTS)?;
                Database::get_event_tx(event_id, &events_table)
            })
            .await
            .expect("Database panic")
    }

    pub async fn get_event_content(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> Option<EventContent> {
        let event_id = event_id.into();
        self.db
            .read_with(|tx| {
                let events_content_table = tx.open_table(&crate::db::TABLE_EVENTS_CONTENT)?;
                Ok(
                    Database::get_event_content_tx(event_id, &events_content_table)?.and_then(
                        |content_state| match content_state {
                            crate::db::events::ContentStateRef::Present(b) => Some(b.into_owned()),
                            crate::db::events::ContentStateRef::Deleted { .. }
                            | crate::db::events::ContentStateRef::Pruned => None,
                        },
                    ),
                )
            })
            .await
            .expect("Database panic")
    }

    pub async fn get_self_current_head(&self) -> Option<ShortEventId> {
        self.db
            .read_with(|tx| {
                let events_heads_table = tx.open_table(&TABLE_EVENTS_HEADS)?;

                Database::get_head_tx(self.self_id, &events_heads_table)
            })
            .await
            .expect("Storage error")
    }

    pub async fn get_self_random_eventid(&self) -> Option<ShortEventId> {
        self.db
            .read_with(|tx| {
                let events_self_table = tx.open_table(&TABLE_EVENTS_SELF)?;

                Database::get_random_self_event(&events_self_table)
            })
            .await
            .expect("Storage error")
    }

    pub async fn process_event(
        &self,
        event: &VerifiedEvent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.db
            .write_with(|tx| self.process_event_tx(event, tx))
            .await
            .expect("Storage error")
    }

    pub async fn process_event_with_content(
        &self,
        event: &VerifiedEvent,
        content: &VerifiedEventContent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.db
            .write_with(|tx| {
                let res = self.process_event_tx(event, tx)?;
                self.process_event_content_tx(content, tx)?;
                Ok(res)
            })
            .await
            .expect("Storage error")
    }

    pub fn process_event_tx(
        &self,
        event: &VerifiedEvent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let mut events_table = tx.open_table(&crate::db::TABLE_EVENTS)?;
        let mut events_content_table = tx.open_table(&crate::db::TABLE_EVENTS_CONTENT)?;
        let mut events_missing_table = tx.open_table(&TABLE_EVENTS_MISSING)?;
        let mut events_heads_table = tx.open_table(&TABLE_EVENTS_HEADS)?;
        let mut events_by_time_table = tx.open_table(&TABLE_EVENTS_BY_TIME)?;

        let insert_event_outcome = Database::insert_event_tx(
            event,
            &mut events_table,
            &mut events_by_time_table,
            &mut events_content_table,
            &mut events_missing_table,
            &mut events_heads_table,
        )?;

        if let InsertEventOutcome::Inserted { was_missing, .. } = insert_event_outcome {
            info!(target: LOG_TARGET,
                event_id = %event.event_id,
                author = %event.event.author,
                parent_prev = %event.event.parent_prev,
                parent_aux = %event.event.parent_aux,
                "New event inserted"
            );
            if event.event.author == self.self_id {
                let mut events_self_table = tx.open_table(&crate::db::TABLE_EVENTS_SELF)?;
                Database::insert_self_event_id(event.event_id, &mut events_self_table)?;

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

        let process_event_content_state =
            if Self::MAX_CONTENT_LEN < u32::from(event.event.content_len) {
                Database::prune_event_content_tx(event.event_id, &mut events_content_table)?;

                ProcessEventState::Pruned
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
        Ok((insert_event_outcome, process_event_content_state))
    }

    /// Process event content
    ///
    /// Note: Must only be called for an event that was already processed
    pub async fn process_event_content(&self, event_content: &VerifiedEventContent) {
        self.db
            .write_with(|tx| self.process_event_content_tx(event_content, tx))
            .await
            .expect("Storage error")
    }

    pub fn process_event_content_tx(
        &self,
        event_content: &VerifiedEventContent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        let events_table = tx.open_table(&crate::db::TABLE_EVENTS)?;
        let mut events_content_table = tx.open_table(&crate::db::TABLE_EVENTS_CONTENT)?;

        debug_assert!(Database::has_event_tx(
            event_content.event_id,
            &events_table
        )?);

        let content_added = if u32::from(event_content.event.content_len) < Self::MAX_CONTENT_LEN {
            Database::insert_event_content_tx(event_content, &mut events_content_table)?
        } else {
            false
        };

        if content_added {
            self.process_event_content_inserted_tx(event_content, tx)?;
        }
        Ok(())
    }

    /// After an event content was inserted process special kinds of event
    /// content, like follows/unfollows
    pub fn process_event_content_inserted_tx(
        &self,
        event_content: &VerifiedEventContent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        let author = event_content.event.author;
        let updated = match event_content.event.kind {
            EventKind::FOLLOW | EventKind::UNFOLLOW => {
                let mut id_followees_table = tx.open_table(&crate::db::TABLE_ID_FOLLOWEES)?;
                let mut id_followers_table = tx.open_table(&crate::db::TABLE_ID_FOLLOWERS)?;
                let mut id_unfollowed_table = tx.open_table(&crate::db::TABLE_ID_UNFOLLOWED)?;

                match event_content.event.kind {
                    EventKind::FOLLOW => match event_content.content.decode::<content::Follow>() {
                        Ok(follow_content) => Database::insert_follow_tx(
                            author,
                            event_content.event.timestamp.into(),
                            follow_content,
                            &mut id_followees_table,
                            &mut id_followers_table,
                            &mut id_unfollowed_table,
                        )?,
                        Err(err) => {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed ContentFollow payload");
                            false
                        }
                    },
                    EventKind::UNFOLLOW => {
                        match event_content.content.decode::<content::Unfollow>() {
                            Ok(unfollow_content) => Database::insert_unfollow_tx(
                                author,
                                event_content.event.timestamp.into(),
                                unfollow_content,
                                &mut id_followees_table,
                                &mut id_followers_table,
                                &mut id_unfollowed_table,
                            )?,
                            Err(err) => {
                                debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed ContentUnfollow payload");
                                false
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            _ => false,
        };

        if updated && author == self.self_id {
            let sender = self.self_followee_list_updated.clone();
            tx.on_commit(move || {
                let _ = sender.send(());
            });
        }
        Ok(())
    }

    pub async fn wants_content(
        &self,
        event_id: impl Into<ShortEventId>,
        process_state: ProcessEventState,
    ) -> bool {
        match process_state.wants_content() {
            ContentWantState::DoesNotWant => {
                return false;
            }
            ContentWantState::Wants => {
                return true;
            }
            ContentWantState::MaybeWants => {}
        }

        self.db
            .read_with(|tx| {
                let events_content_table = tx.open_table(&crate::db::TABLE_EVENTS_CONTENT)?;

                Database::has_event_content_tx(event_id.into(), &events_content_table)
            })
            .await
            .expect("Storage error")
    }

    pub fn iroh_secret(&self) -> iroh::SecretKey {
        self.iroh_secret.clone()
    }
}

pub enum ProcessEventState {
    New,
    Existing,
    Pruned,
    Deleted,
}

pub enum ContentWantState {
    Wants,
    MaybeWants,
    DoesNotWant,
}

impl ProcessEventState {
    pub fn wants_content(self) -> ContentWantState {
        match self {
            ProcessEventState::New => ContentWantState::Wants,
            ProcessEventState::Existing => ContentWantState::MaybeWants,
            ProcessEventState::Pruned => ContentWantState::DoesNotWant,
            ProcessEventState::Deleted => ContentWantState::DoesNotWant,
        }
    }
}
