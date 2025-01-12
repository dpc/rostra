use rostra_core::event::{SignedEvent, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use tokio::sync::watch;

use crate::db::{Database, DbResult, EventState, TABLE_EVENTS_HEADS, TABLE_EVENTS_MISSING};

pub struct Storage {
    db: Database,
    self_followee_list_updated: watch::Sender<Vec<RostraId>>,
}

pub enum EventContentState {
    Missing,
    Present,
    Pruned,
    Deleted,
}

impl Storage {
    pub async fn new(db: Database, self_id: RostraId) -> DbResult<Self> {
        let self_followees = db
            .read_followees(self_id.into())
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let (self_followee_list_updated, _) = watch::channel(self_followees);
        Ok(Self {
            db,
            self_followee_list_updated,
        })
    }

    pub fn self_followees_list_subscribe(&self) -> watch::Receiver<Vec<RostraId>> {
        self.self_followee_list_updated.subscribe()
    }

    pub async fn has_event(&self, event_id: impl Into<ShortEventId>) -> bool {
        let event_id = event_id.into();
        self.db
            .read_with(|tx| {
                let events_table = tx
                    .open_table(&crate::db::TABLE_EVENTS)
                    .expect("Storage error");
                Ok(Database::has_event_tx(event_id, &events_table)?)
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
                Ok(Database::get_event_tx(event_id, &events_table)?)
            })
            .await
            .expect("Database panic")
    }

    pub async fn process_event(&self, event: &VerifiedEvent) -> ProcessEventOutcome {
        self.db
            .write_with(|tx| {
                let mut events_table = tx.open_table(&crate::db::TABLE_EVENTS)?;
                let mut events_content_table = tx.open_table(&crate::db::TABLE_EVENTS_CONTENT)?;
                let mut events_missing_table = tx.open_table(&TABLE_EVENTS_MISSING)?;
                let mut events_heads_table = tx.open_table(&TABLE_EVENTS_HEADS)?;

                let event_state = Database::insert_event_tx(
                    event,
                    &mut events_table,
                    &mut events_content_table,
                    &mut events_missing_table,
                    &mut events_heads_table,
                )?;

                Ok(if 1_000_000u32 < u32::from(event.event.content_len) {
                    Database::prune_event_content_tx(event.event_id, &mut events_content_table)?;

                    ProcessEventOutcome::DoesnotNeedContent
                } else {
                    match event_state {
                        EventState::AlreadyPresent => ProcessEventOutcome::MaybeNeedsContent,
                        EventState::InsertedDeleted => ProcessEventOutcome::DoesnotNeedContent,
                        EventState::Inserted => ProcessEventOutcome::NeedsContent,
                    }
                })
            })
            .await
            .expect("Storage error")
    }
}

enum ProcessEventOutcome {
    NeedsContent,
    MaybeNeedsContent,
    DoesnotNeedContent,
}
