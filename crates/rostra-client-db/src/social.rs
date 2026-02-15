use std::collections::{BTreeMap, HashMap};

use bincode::{Decode, Encode};
use rostra_core::event::{EventExt as _, PersonaId, SocialPost, content_kind};
use rostra_core::id::RostraId;
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::Database;
use crate::event::ContentStoreRecord;
use crate::{
    DbResult, LOG_TARGET, content_store, events, events_content_state,
    shoutbox_posts_by_received_at, social_posts, social_posts_by_received_at, social_posts_by_time,
    social_posts_reactions, social_posts_replies, tables,
};

/// Cursor for paginating events by their author timestamp.
///
/// Used for Followees and Network timeline tabs where posts are ordered
/// by when they were authored, not when we received them.
/// Key structure: `(author_timestamp, event_id)`
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

/// Cursor for paginating events by when we received them.
///
/// Used for Notifications tab where posts are ordered by reception time,
/// not author timestamp. Includes a monotonic counter for strict ordering
/// when multiple events arrive at the same timestamp.
/// Key structure: `(received_timestamp, seq)`
#[derive(
    Encode, Decode, Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct ReceivedAtPaginationCursor {
    pub ts: Timestamp,
    /// Monotonic sequence number for ordering events at the same timestamp.
    /// Alias for backwards compatibility with cookies saved as
    /// "reception_order".
    #[serde(alias = "reception_order")]
    pub seq: u64,
}

impl ReceivedAtPaginationCursor {
    pub const ZERO: Self = Self {
        ts: Timestamp::ZERO,
        seq: 0,
    };
    pub const MAX: Self = Self {
        ts: Timestamp::MAX,
        seq: u64::MAX,
    };

    /// Returns the next cursor position (exclusive start for forward
    /// iteration).
    ///
    /// Used when we want to iterate starting AFTER this cursor position.
    pub fn next(self) -> Self {
        if self.seq < u64::MAX {
            Self {
                ts: self.ts,
                seq: self.seq + 1,
            }
        } else {
            // seq overflow, increment timestamp
            Self {
                ts: Timestamp::from(u64::from(self.ts).saturating_add(1)),
                seq: 0,
            }
        }
    }
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

/// Record for a shoutbox post with associated metadata.
#[derive(Clone, Debug)]
pub struct ShoutboxPostRecord {
    pub received_ts: Timestamp,
    pub event_id: ShortEventId,
    pub author: RostraId,
    pub content: content_kind::Shoutbox,
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
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let (ret, cursor) = Self::paginate_table(&social_posts_by_time_table,
                cursor.map(|c| (c.ts, c.event_id)),
                limit,
                move |(ts, event_id), _| {

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };
                let content_hash = event.content_hash();

                // If event has a content state, it means content is deleted or pruned
                if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                    .is_some()
                {
                    return Ok(None);
                }

                // Get content from store
                let Some(store_record) =
                    content_store_table.get(&content_hash)?.map(|g| g.value())
                else {
                    return Ok(None);
                };
                let ContentStoreRecord::Present(content) = store_record else {
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Content invalid");
                    return Ok(None);
                };

                let social_post_record =
                    Database::get_social_post_tx(event_id, &social_posts_table)?.unwrap_or_default();

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

            Ok((
                ret,
                cursor.map(|(ts, event_id)| EventPaginationCursor { ts, event_id }),
            ))
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
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let (ret, cursor) = Self::paginate_table_rev(&social_posts_by_time_table,
                cursor.map(|c| (c.ts, c.event_id)),
                limit,
                move |(ts, event_id), _| {

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };
                let content_hash = event.content_hash();

                // If event has a content state, it means content is deleted or pruned
                if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                    .is_some()
                {
                    return Ok(None);
                }

                // Get content from store
                let Some(store_record) =
                    content_store_table.get(&content_hash)?.map(|g| g.value())
                else {
                    return Ok(None);
                };
                let ContentStoreRecord::Present(content) = store_record else {
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Content invalid");
                    return Ok(None);
                };

                let social_post_record =
                    Database::get_social_post_tx(event_id, &social_posts_table)?.unwrap_or_default();

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

            Ok((
                ret,
                cursor.map(|(ts, event_id)| EventPaginationCursor { ts, event_id }),
            ))
        })
        .await
        .expect("Storage error")
    }

    /// Get the cursor of the most recently received social post.
    ///
    /// Used for tracking "last seen" notification position.
    pub async fn get_latest_social_post_received_at_cursor(
        &self,
    ) -> Option<ReceivedAtPaginationCursor> {
        self.read_with(|tx| {
            let social_posts_by_received_at_table =
                tx.open_table(&social_posts_by_received_at::TABLE)?;

            // Get the last (most recent) entry in the table
            if let Some(entry) = social_posts_by_received_at_table.last()? {
                let (ts, seq) = entry.0.value();
                Ok(Some(ReceivedAtPaginationCursor { ts, seq }))
            } else {
                Ok(None)
            }
        })
        .await
        .expect("Storage error")
    }

    /// Paginate social posts ordered by when we received them (forward).
    ///
    /// Used for notification badge count calculation.
    pub async fn paginate_social_posts_by_received_at(
        &self,
        cursor: Option<ReceivedAtPaginationCursor>,
        limit: usize,
        filter_fn: impl Fn(&SocialPostRecord<SocialPost>) -> bool + Send + 'static,
    ) -> (
        Vec<SocialPostRecord<content_kind::SocialPost>>,
        Option<ReceivedAtPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let social_posts_by_received_at_table =
                tx.open_table(&social_posts_by_received_at::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let (ret, cursor) = Self::paginate_table(
                &social_posts_by_received_at_table,
                cursor.map(|c| (c.ts, c.seq)),
                limit,
                move |(ts, _seq), event_id| {
                    let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                        warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                        return Ok(None);
                    };
                    let content_hash = event.content_hash();

                    // If event has a content state, it means content is deleted or pruned
                    if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                        .is_some()
                    {
                        return Ok(None);
                    }

                    // Get content from store
                    let Some(store_record) =
                        content_store_table.get(&content_hash)?.map(|g| g.value())
                    else {
                        return Ok(None);
                    };
                    let ContentStoreRecord::Present(content) = store_record else {
                        return Ok(None);
                    };

                    let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>()
                    else {
                        debug!(target: LOG_TARGET, %event_id, "Content invalid");
                        return Ok(None);
                    };

                    let social_post_record =
                        Database::get_social_post_tx(event_id, &social_posts_table)?
                            .unwrap_or_default();

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
                },
            )?;

            Ok((
                ret,
                cursor.map(|(ts, seq)| ReceivedAtPaginationCursor {
                    ts,
                    seq,
                }),
            ))
        })
        .await
        .expect("Storage error")
    }

    /// Paginate social posts ordered by when we received them (reverse).
    ///
    /// Used for notification timeline display.
    pub async fn paginate_social_posts_by_received_at_rev(
        &self,
        cursor: Option<ReceivedAtPaginationCursor>,
        limit: usize,
        filter_fn: impl Fn(&SocialPostRecord<SocialPost>) -> bool + Send + 'static,
    ) -> (
        Vec<SocialPostRecord<content_kind::SocialPost>>,
        Option<ReceivedAtPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let social_posts_by_received_at_table =
                tx.open_table(&social_posts_by_received_at::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let (ret, cursor) = Self::paginate_table_rev(
                &social_posts_by_received_at_table,
                cursor.map(|c| (c.ts, c.seq)),
                limit,
                move |(ts, _seq), event_id| {
                    let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                        warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                        return Ok(None);
                    };
                    let content_hash = event.content_hash();

                    // If event has a content state, it means content is deleted or pruned
                    if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                        .is_some()
                    {
                        return Ok(None);
                    }

                    // Get content from store
                    let Some(store_record) =
                        content_store_table.get(&content_hash)?.map(|g| g.value())
                    else {
                        return Ok(None);
                    };
                    let ContentStoreRecord::Present(content) = store_record else {
                        return Ok(None);
                    };

                    let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>()
                    else {
                        debug!(target: LOG_TARGET, %event_id, "Content invalid");
                        return Ok(None);
                    };

                    let social_post_record =
                        Database::get_social_post_tx(event_id, &social_posts_table)?
                            .unwrap_or_default();

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
                },
            )?;

            Ok((
                ret,
                cursor.map(|(ts, seq)| ReceivedAtPaginationCursor {
                    ts,
                    seq,
                }),
            ))
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
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let (ret, cursor) = Database::paginate_table_partition_rev(&social_post_replies_tbl,
                (post_event_id, Timestamp::ZERO, ShortEventId::ZERO)..=
                (post_event_id, Timestamp::MAX, ShortEventId::MAX),
                |(_, ts, event_id)| (post_event_id, ts, event_id),

                 cursor.map(|c| (post_event_id, c.ts, c.event_id)), limit, move |(_, ts, event_id), _| {

                let social_post_record = Database::get_social_post_tx(event_id, &social_posts_tbl)?.unwrap_or_default();

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };
                let content_hash = event.content_hash();

                // If event has a content state, it means content is deleted or pruned
                if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                    .is_some()
                {
                    debug!(target: LOG_TARGET, %event_id, "Skipping comment without content present");
                    return Ok(None);
                }

                // Get content from store
                let Some(store_record) = content_store_table.get(&content_hash)?.map(|g| g.value()) else {
                    debug!(target: LOG_TARGET, %event_id, "Skipping comment without content present");
                    return Ok(None);
                };
                let ContentStoreRecord::Present(content) = store_record else {
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Skpping comment with invalid content");
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

            Ok((ret, cursor.map(|(_, ts, event_id)| EventPaginationCursor { ts, event_id })))
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
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;


            let (ret, cursor) = Database::paginate_table_partition_rev(&social_post_reactions_tbl,
                (post_event_id, Timestamp::ZERO, ShortEventId::ZERO)..=(post_event_id, Timestamp::MAX, ShortEventId::MAX),
                |(_, ts, event_id)| (post_event_id, ts, event_id),

                cursor.map(|c| (post_event_id, c.ts, c.event_id)), limit, move |(_, ts, event_id), _| {

                let social_post_record = Database::get_social_post_tx(event_id, &social_posts_tbl)?.unwrap_or_default();

                let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                    warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
                    return Ok(None);
                };
                let content_hash = event.content_hash();

                // If event has a content state, it means content is deleted or pruned
                if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                    .is_some()
                {
                    debug!(target: LOG_TARGET, %event_id, "Skipping reaction without content present");
                    return Ok(None);
                }

                // Get content from store
                let Some(store_record) = content_store_table.get(&content_hash)?.map(|g| g.value()) else {
                    debug!(target: LOG_TARGET, %event_id, "Skipping comment without content present");
                    return Ok(None);
                };
                let ContentStoreRecord::Present(content) = store_record else {
                    return Ok(None);
                };

                let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
                    debug!(target: LOG_TARGET, %event_id, "Skpping comment with invalid content");
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
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let mut ret = HashMap::new();

            for event_id in post_ids {
                let Some((social_post, event, social_post_record)) =
                    Self::get_social_post_record_tx(
                        &events_table,
                        &social_posts_table,
                        &events_content_state_table,
                        &content_store_table,
                        event_id,
                    )?
                else {
                    continue;
                };

                ret.insert(
                    event_id,
                    SocialPostRecord {
                        ts: event.timestamp(),
                        author: event.author(),
                        event_id,
                        reply_count: social_post_record.reply_count,
                        reply_to: social_post.reply_to,
                        content: social_post,
                    },
                );
            }

            Ok(ret)
        })
        .await
        .expect("Storage error")
    }
    pub async fn get_personas_for_id(&self, id: RostraId) -> BTreeMap<PersonaId, String> {
        self.read_with(|tx| {
            let personas = tx.open_table(&tables::ids_personas::TABLE)?;

            // Default predefined personas
            let mut ret = BTreeMap::from([
                (PersonaId(0), "Personal".into()),
                (PersonaId(1), "Professional".into()),
                (PersonaId(2), "Civic".into()),
            ]);

            for record in personas.range(&(id, PersonaId::MIN)..=&(id, PersonaId::MAX))? {
                let (k, v) = record?;
                ret.insert(k.value().1, v.value().display_name);
            }

            Ok(ret)
        })
        .await
        .expect("Storage error")
    }

    pub async fn get_personas(
        &self,
        iter: impl Iterator<Item = (RostraId, PersonaId)>,
    ) -> BTreeMap<(RostraId, PersonaId), String> {
        self.read_with(|tx| {
            let personas = tx.open_table(&tables::ids_personas::TABLE)?;

            // Default predefined personas
            let default_personas: BTreeMap<PersonaId, String> = BTreeMap::from([
                (PersonaId(0), "Personal".into()),
                (PersonaId(1), "Professional".into()),
                (PersonaId(2), "Civic".into()),
            ]);

            let mut ret = BTreeMap::new();
            for (rostra_id, persona_id) in iter {
                if let Some(record) = personas.get(&(rostra_id, persona_id))? {
                    ret.insert((rostra_id, persona_id), record.value().display_name);
                } else {
                    if let Some(d) = default_personas.get(&persona_id) {
                        ret.insert((rostra_id, persona_id), d.clone());
                    }
                }
            }
            Ok(ret)
        })
        .await
        .expect("Storage error")
    }

    pub async fn get_social_post(
        &self,
        event_id: ShortEventId,
    ) -> Option<SocialPostRecord<content_kind::SocialPost>> {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let Some((social_post, event, social_post_record)) = Self::get_social_post_record_tx(
                &events_table,
                &social_posts_table,
                &events_content_state_table,
                &content_store_table,
                event_id,
            )?
            else {
                return Ok(None);
            };

            Ok(Some(SocialPostRecord {
                ts: event.timestamp(),
                author: event.author(),
                event_id,
                reply_count: social_post_record.reply_count,
                reply_to: social_post.reply_to,
                content: social_post,
            }))
        })
        .await
        .expect("Storage error")
    }

    fn get_social_post_record_tx(
        events_table: &impl events::ReadableTable,
        social_posts_table: &impl social_posts::ReadableTable,
        events_content_state_table: &impl events_content_state::ReadableTable,
        content_store_table: &impl content_store::ReadableTable,
        event_id: ShortEventId,
    ) -> DbResult<Option<(SocialPost, crate::EventRecord, crate::SocialPostRecord)>> {
        // Get event first to find content_hash
        let Some(event) = Database::get_event_tx(event_id, events_table)? else {
            warn!(target: LOG_TARGET, %event_id, "Missing event for a post with social_post_record?!");
            return Ok(None);
        };
        let content_hash = event.content_hash();

        // If event has a content state, it means content is deleted or pruned
        if Database::get_event_content_state_tx(event_id, events_content_state_table)?.is_some() {
            return Ok(None);
        }

        // Look up content from store
        let Some(store_record) = content_store_table.get(&content_hash)?.map(|g| g.value()) else {
            return Ok(None);
        };

        let ContentStoreRecord::Present(content) = store_record else {
            return Ok(None);
        };

        let Ok(social_post) = content.deserialize_cbor::<content_kind::SocialPost>() else {
            debug!(target: LOG_TARGET, %event_id, "Content invalid");
            return Ok(None);
        };
        let social_post_record =
            Database::get_social_post_tx(event_id, social_posts_table)?.unwrap_or_default();
        Ok(Some((social_post, event, social_post_record)))
    }

    /// Check if a post is a self-mention (mentions the local user).
    ///
    /// Returns `true` if the post with the given event_id is recorded in
    /// the `social_posts_self_mention` table, meaning it contains an @mention
    /// of the local user.
    pub async fn is_self_mention(&self, event_id: ShortEventId) -> bool {
        self.read_with(|tx| {
            let self_mention_table = tx.open_table(&crate::social_posts_self_mention::TABLE)?;
            Ok(self_mention_table.get(&event_id)?.is_some())
        })
        .await
        .unwrap_or(false)
    }

    /// Get the set of all event IDs that are self-mentions.
    ///
    /// Returns a HashSet of ShortEventIds for posts that mention the local
    /// user. Used for efficient filtering in notification queries.
    pub async fn get_self_mentions(&self) -> std::collections::HashSet<ShortEventId> {
        self.read_with(|tx| {
            let self_mention_table = tx.open_table(&crate::social_posts_self_mention::TABLE)?;
            let mut mentions = std::collections::HashSet::new();
            for entry in self_mention_table.range(..)? {
                let (key, _) = entry?;
                mentions.insert(key.value());
            }
            Ok(mentions)
        })
        .await
        .unwrap_or_default()
    }

    /// Get the cursor of the most recently received shoutbox post.
    ///
    /// Used for tracking "last seen" shoutbox position.
    pub async fn get_latest_shoutbox_received_at_cursor(
        &self,
    ) -> Option<ReceivedAtPaginationCursor> {
        self.read_with(|tx| {
            let shoutbox_by_received_at_table =
                tx.open_table(&shoutbox_posts_by_received_at::TABLE)?;

            // Get the last (most recent) entry in the table
            if let Some(entry) = shoutbox_by_received_at_table.last()? {
                let (ts, seq) = entry.0.value();
                Ok(Some(ReceivedAtPaginationCursor { ts, seq }))
            } else {
                Ok(None)
            }
        })
        .await
        .expect("Storage error")
    }

    /// Paginate shoutbox posts ordered by when we received them (reverse -
    /// newest first).
    ///
    /// Unlike social posts, shoutbox posts are not filtered - all posts are
    /// shown.
    pub async fn paginate_shoutbox_posts_by_received_at_rev(
        &self,
        cursor: Option<ReceivedAtPaginationCursor>,
        limit: usize,
    ) -> (Vec<ShoutboxPostRecord>, Option<ReceivedAtPaginationCursor>) {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let shoutbox_by_received_at_table =
                tx.open_table(&shoutbox_posts_by_received_at::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            let (ret, cursor) = Self::paginate_table_rev(
                &shoutbox_by_received_at_table,
                cursor.map(|c| (c.ts, c.seq)),
                limit,
                move |(ts, _seq), event_id| {
                    let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                        warn!(target: LOG_TARGET, %event_id, "Missing event for shoutbox post");
                        return Ok(None);
                    };
                    let content_hash = event.content_hash();

                    // If event has a content state, it means content is deleted or pruned
                    if Database::get_event_content_state_tx(event_id, &events_content_state_table)?
                        .is_some()
                    {
                        return Ok(None);
                    }

                    // Get content from store
                    let Some(store_record) =
                        content_store_table.get(&content_hash)?.map(|g| g.value())
                    else {
                        return Ok(None);
                    };
                    let ContentStoreRecord::Present(content) = store_record else {
                        return Ok(None);
                    };

                    let Ok(shoutbox) = content.deserialize_cbor::<content_kind::Shoutbox>() else {
                        debug!(target: LOG_TARGET, %event_id, "Shoutbox content invalid");
                        return Ok(None);
                    };

                    Ok(Some(ShoutboxPostRecord {
                        received_ts: ts,
                        author: event.author(),
                        event_id,
                        content: shoutbox,
                    }))
                },
            )?;

            Ok((
                ret,
                cursor.map(|(ts, seq)| ReceivedAtPaginationCursor { ts, seq }),
            ))
        })
        .await
        .expect("Storage error")
    }

    /// Count shoutbox posts received after the given cursor.
    ///
    /// Used for counting unread shoutbox messages.
    pub async fn count_shoutbox_posts_since(
        &self,
        cursor: Option<ReceivedAtPaginationCursor>,
        limit: usize,
    ) -> usize {
        self.read_with(|tx| {
            let shoutbox_by_received_at_table =
                tx.open_table(&shoutbox_posts_by_received_at::TABLE)?;

            // Use cursor.next() to start AFTER the cursor
            let start = cursor.map(|c| c.next());
            let mut count = 0;

            for entry in if let Some(start) = start {
                shoutbox_by_received_at_table.range(&(start.ts, start.seq)..)?
            } else {
                shoutbox_by_received_at_table.range(..)?
            } {
                let _ = entry?;
                count += 1;
                if limit <= count {
                    break;
                }
            }

            Ok(count)
        })
        .await
        .expect("Storage error")
    }
}
