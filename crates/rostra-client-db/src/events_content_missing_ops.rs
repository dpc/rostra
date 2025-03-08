use rostra_core::event::EventExt as _;
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use tracing::warn;

use crate::{events, tables, Database, LOG_TARGET};

impl Database {
    pub async fn paginate_missing_events_contents(
        &self,
        cursor: Option<ShortEventId>,
        limit: usize,
    ) -> (Vec<(RostraId, ShortEventId)>, Option<ShortEventId>) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_content_missing_table = tx.open_table(&tables::events_content_missing::TABLE)?;

            Self::paginate_table(&events_content_missing_table,
                cursor,
                limit,
                move |event_id, _| {

                let Some(event) =
                events_table.get(&event_id)?.map(|e| e.value())
                else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for content_missing event?!");
                    return Ok(None);
                };

                Ok(Some((event.signed.author(), event_id)))
            })


        })
        .await
        .expect("Storage error")
    }
}
