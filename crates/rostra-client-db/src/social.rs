use bincode::{Decode, Encode};
use rostra_core::event::{content_kind, Event, EventKind};
use rostra_core::{ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::Database;
use crate::event::EventContentState;
use crate::{events, events_by_time, events_content, LOG_TARGET};

#[derive(
    Encode, Decode, Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct EventPaginationCursor {
    ts: Timestamp,
    event_id: ShortEventId,
}

pub struct EventPaginationRecord<C> {
    pub ts: Timestamp,
    pub event_id: ShortEventId,
    pub event: Event,
    pub content: C,
}

impl Database {
    pub async fn paginate_social_posts_rev(
        &self,
        upper_bound: Option<EventPaginationCursor>,
        limit: usize,
    ) -> Vec<EventPaginationRecord<content_kind::SocialPost>> {
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
                let (k, _) = event?;
                let (ts, event_id) = k.value();

                let Some(e_record) = Database::get_event_tx(event_id, &events_table)? else {
                    continue;
                };

                if e_record.signed.event.kind != EventKind::SOCIAL_POST {
                    continue;
                }

                let Some(content_state) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    continue;
                };
                let EventContentState::Present(content) = content_state else {
                    continue;
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Content invalid");
                    continue;
                };

                debug_assert_eq!(Timestamp::from(e_record.signed.event.timestamp), ts);

                ret.push(EventPaginationRecord {
                    ts,
                    event_id,
                    event: e_record.signed.event,
                    content: social_post,
                });

                if limit <= ret.len() {
                    break;
                }
            }

            Ok(ret)
        })
        .await
        .expect("Storage error")
    }
}
