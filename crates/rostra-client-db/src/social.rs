use std::collections::HashMap;

use bincode::{Decode, Encode};
use rostra_core::event::{content_kind, EventExt as _, SocialPost};
use rostra_core::id::RostraId;
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::Database;
use crate::event::EventContentState;
use crate::{
    events, events_content, social_posts, social_posts_by_time, social_posts_reactions,
    social_posts_replies, LOG_TARGET,
};

#[derive(
    Encode, Decode, Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct EventPaginationCursor {
    pub ts: Timestamp,
    pub event_id: ShortEventId,
}

impl EventPaginationCursor {
    pub const ZERO: Self = Self {
        ts: Timestamp::ZERO,
        event_id: ShortEventId::ZERO,
    };
    pub const MAX: Self = Self {
        ts: Timestamp::MAX,
        event_id: ShortEventId::MAX,
    };
}
#[derive(Clone, Debug)]
pub struct SocialPostRecord<C> {
    pub ts: Timestamp,
    pub event_id: ShortEventId,
    pub author: RostraId,
    pub reply_to: Option<ExternalEventId>,
    pub content: C,
    pub reply_count: u64,
}

impl Database {
    pub async fn paginate_social_posts(
        &self,
        cursor: Option<EventPaginationCursor>,
        limit: usize,
        filter_fn: impl Fn(&SocialPostRecord<SocialPost>) -> bool + Send + 'static,
    ) -> (
        Vec<SocialPostRecord<content_kind::SocialPost>>,
        Option<EventPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let social_posts_by_time_table = tx.open_table(&social_posts_by_time::TABLE)?;
            let events_content_table = tx.open_table(&events_content::TABLE)?;

            let (ret, cursor) = Self::paginate_table(&social_posts_by_time_table,
                cursor.map(|c| (c.ts, c.event_id)),
                limit,
                move |(ts, event_id), _| {

                let Some(content_state) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    warn!(target: LOG_TARGET, %event_id, "Missing content for a post with social_post_record?!");
                    return Ok(None);
                };
                let EventContentState::Present(content) = content_state else {
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Content invalid");
                    return Ok(None);
                };

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };

                let social_post_record =
                    Database::get_social_post_tx(event_id, &social_posts_table)?.unwrap_or_default()
                ;

                let social_post_record = SocialPostRecord {
                    ts,
                    author: event.author(),
                    event_id,
                    reply_count: social_post_record.reply_count,
                    reply_to: social_post.reply_to,
                    content: social_post,
                };

                if !filter_fn(&social_post_record) {
                    return Ok(None);
                }

                Ok(Some(social_post_record))
            })?;


            Ok((ret, cursor.map(|(ts, event_id)| EventPaginationCursor{ ts, event_id })))
        })
        .await
        .expect("Storage error")
    }

    pub async fn paginate_social_posts_rev(
        &self,
        cursor: Option<EventPaginationCursor>,
        limit: usize,
        filter_fn: impl Fn(&SocialPostRecord<SocialPost>) -> bool + Send + 'static,
    ) -> (
        Vec<SocialPostRecord<content_kind::SocialPost>>,
        Option<EventPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let social_posts_by_time_table = tx.open_table(&social_posts_by_time::TABLE)?;
            let events_content_table = tx.open_table(&events_content::TABLE)?;

            let (ret, cursor) = Self::paginate_table_rev(&social_posts_by_time_table,
                cursor.map(|c| (c.ts, c.event_id)),
                limit,
                move |(ts, event_id), _| {

                let Some(content_state) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    warn!(target: LOG_TARGET, %event_id, "Missing content for a post with social_post_record?!");
                    return Ok(None);
                };
                let EventContentState::Present(content) = content_state else {
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Content invalid");
                    return Ok(None);
                };

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };

                let social_post_record =
                    Database::get_social_post_tx(event_id, &social_posts_table)?.unwrap_or_default()
                ;

                let social_post_record = SocialPostRecord {
                    ts,
                    author: event.author(),
                    event_id,
                    reply_count: social_post_record.reply_count,
                    reply_to: social_post.reply_to,
                    content: social_post,
                };

                if !filter_fn(&social_post_record) {
                    return Ok(None);
                }

                Ok(Some(social_post_record))
            })?;


            Ok((ret, cursor.map(|(ts, event_id)| EventPaginationCursor{ ts, event_id })))
        })
        .await
        .expect("Storage error")
    }

    pub async fn paginate_social_post_comments_rev(
        &self,
        post_event_id: ShortEventId,
        cursor: Option<EventPaginationCursor>,
        limit: usize,
    ) -> (
        Vec<SocialPostRecord<content_kind::SocialPost>>,
        Option<EventPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_tbl = tx.open_table(&social_posts::TABLE)?;
            let social_post_replies_tbl = tx.open_table(&social_posts_replies::TABLE)?;
            let events_content_table = tx.open_table(&events_content::TABLE)?;

            let (ret, cursor) = Database::paginate_table_partition_rev(&social_post_replies_tbl,
                (post_event_id, Timestamp::ZERO, ShortEventId::ZERO)..=
                (post_event_id, Timestamp::MAX, ShortEventId::MAX),
                |(_, ts, event_id)| (post_event_id, ts, event_id),

                 cursor.map(|c| (post_event_id, c.ts, c.event_id)), limit, move |(_, ts, event_id), _| {

                let social_post_record = Database::get_social_post_tx(event_id, &social_posts_tbl)?.unwrap_or_default();

                let Some(content_state) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    warn!(target: LOG_TARGET, %event_id, "Missing content for a post with social_post_record?!");
                    return Ok(None);
                };
                let EventContentState::Present(content) = content_state else {
                    debug!(target: LOG_TARGET, %event_id, "Skipping comment without content present");
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Skpping comment with invalid content");
                    return Ok(None);
                };

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };

                Ok(Some(SocialPostRecord {
                    ts,
                    author: event.author(),
                    event_id,
                    reply_to: social_post.reply_to,
                    reply_count: social_post_record.reply_count,
                    content: social_post,
                }))
            })?;

            Ok((ret, cursor.map(|(_, ts, event_id)| EventPaginationCursor { ts, event_id})))
        })
        .await
        .expect("Storage error")
    }

    pub async fn paginate_social_post_reactions_rev(
        &self,
        post_event_id: ShortEventId,
        cursor: Option<EventPaginationCursor>,
        limit: usize,
    ) -> (
        Vec<SocialPostRecord<content_kind::SocialPost>>,
        Option<EventPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_tbl = tx.open_table(&social_posts::TABLE)?;
            let social_post_reactions_tbl = tx.open_table(&social_posts_reactions::TABLE)?;
            let events_content_table = tx.open_table(&events_content::TABLE)?;


            let (ret, cursor) = Database::paginate_table_partition_rev(&social_post_reactions_tbl,
                (post_event_id, Timestamp::ZERO, ShortEventId::ZERO)..=(post_event_id, Timestamp::MAX, ShortEventId::MAX),
                |(_, ts, event_id)| (post_event_id, ts, event_id),

                cursor.map(|c| (post_event_id, c.ts, c.event_id)), limit, move |(_, ts, event_id), _| {

                let social_post_record = Database::get_social_post_tx(event_id, &social_posts_tbl)?.unwrap_or_default();

                let Some(content_state) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    warn!(target: LOG_TARGET, %event_id, "Missing content for a post with social_post_record?!");
                    return Ok(None);
                };
                let EventContentState::Present(content) = content_state else {
                    debug!(target: LOG_TARGET, %event_id, "Skipping comment without content present");
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Skpping comment with invalid content");
                    return Ok(None);
                };

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };

                Ok(Some(SocialPostRecord {
                    ts,
                    author: event.author(),
                    event_id,
                    reply_to: social_post.reply_to,
                    reply_count: social_post_record.reply_count,
                    content: social_post,
                }))
            })?;

            Ok((ret, cursor.map(|(_, ts, event_id)| EventPaginationCursor { ts, event_id})))
        })
        .await
        .expect("Storage error")
    }

    pub async fn get_posts_by_id(
        &self,
        post_ids: impl Iterator<Item = ShortEventId>,
    ) -> HashMap<ShortEventId, SocialPostRecord<content_kind::SocialPost>> {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let events_content_table = tx.open_table(&events_content::TABLE)?;

            let mut ret = HashMap::new();

            for event_id in post_ids {
                let Some(content_state) =
                    Database::get_event_content_tx(event_id, &events_content_table)?
                else {
                    warn!(target: LOG_TARGET, %event_id, "Missing content for a post with social_post_record?!");
                    continue;
                };
                let EventContentState::Present(content) = content_state else {
                    continue;
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Content invalid");
                    continue;
                };

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    continue;
                };

                let social_post_record =
                    Database::get_social_post_tx(event_id, &social_posts_table)?.unwrap_or_default();

                ret.insert(event_id, SocialPostRecord {
                    ts: event.timestamp(),
                    author: event.author(),
                    event_id,
                    reply_count: social_post_record.reply_count,
                    reply_to: social_post.reply_to,
                    content: social_post,
                });
            }

            Ok(ret)
        })
        .await
        .expect("Storage error")
    }
}
