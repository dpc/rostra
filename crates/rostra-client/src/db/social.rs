use bincode::{Decode, Encode};
use rostra_core::event::{content, Event, EventKind};
use rostra_core::{ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};

use super::Database;
use crate::db::{events, events_by_time, events_content};

#[derive(
    Encode, Decode, Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct EventPaginationCursor {
    ts: Timestamp,
    event_id: ShortEventId,
}

pub struct EventPaginationRecord<C> {
    ts: Timestamp,
    event_id: ShortEventId,
    event: Event,
    content: C,
}

impl Database {
    pub async fn paginate_social_posts_rev(
        &self,
        upper_bound: Option<EventPaginationCursor>,
        limit: usize,
    ) -> Vec<EventPaginationRecord<content::SocialPost>> {
        let upper_bound = upper_bound
            .map(|b| (b.ts, b.event_id))
            .unwrap_or((Timestamp::MAX, ShortEventId::MAX));
        self.read_with(|tx| {
            let events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
            let events_table = tx.open_table(&events::TABLE)?;
            let events_content_table = tx.open_table(&events_content::TABLE)?;

            let mut ret = vec![];

            for event in events_by_time_table
                .range(&(Timestamp::ZERO, ShortEventId::ZERO)..&upper_bound)?
                .rev()
            {
                let (k, v) = event?;
                let (ts, event_id) = k.value();

                let Some(e_record) = Database::get_event_tx(event_id, &events_table)? else {
                    continue;
                };

                if e_record.signed.event.kind != EventKind::SOCIAL_POST {
                    continue;
                }

                let Some(_content) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    continue;
                };

                todo!()
            }

            Ok(ret)
        })
        .await
        .expect("Storage error")
    }
}
