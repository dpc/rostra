use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use ids::{IdsFollowersRecord, IdsUnfollowedRecord};
use rand::{thread_rng, Rng as _};
use redb_bincode::{ReadableTable, Table};
use rostra_core::event::{
    content_kind, EventContent, EventExt as _, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use tables::event::{EventContentState, EventsMissingRecord};
use tables::ids::IdsFolloweesRecord;
use tables::EventRecord;
use tracing::{debug, info};

use super::id_self::IdSelfAccountRecord;
use super::{
    db_version, events, events_by_time, events_content, events_heads, events_missing, events_self,
    get_first_in_range, get_last_in_range, ids, ids_followees, ids_followers, ids_personas,
    ids_self, ids_unfollowed, tables, Database, DbError, DbResult, EventsHeadsTableRecord,
    InsertEventOutcome, WriteTransactionCtx,
};
use crate::{
    ids_full, social_posts, social_posts_by_time, social_posts_reply, social_profiles,
    DbVersionTooHighSnafu, IdSocialProfileRecord, Latest, SocialPostRecord, LOG_TARGET,
};

impl Database {
    pub(crate) fn init_tables_tx(tx: &WriteTransactionCtx) -> DbResult<()> {
        tx.open_table(&db_version::TABLE)?;

        tx.open_table(&ids_self::TABLE)?;
        tx.open_table(&ids_full::TABLE)?;
        tx.open_table(&ids_followers::TABLE)?;
        tx.open_table(&ids_followees::TABLE)?;
        tx.open_table(&ids_unfollowed::TABLE)?;
        tx.open_table(&ids_personas::TABLE)?;

        tx.open_table(&events::TABLE)?;
        tx.open_table(&events_by_time::TABLE)?;
        tx.open_table(&events_content::TABLE)?;
        tx.open_table(&events_self::TABLE)?;
        tx.open_table(&events_missing::TABLE)?;
        tx.open_table(&events_heads::TABLE)?;

        tx.open_table(&social_profiles::TABLE)?;
        tx.open_table(&social_posts::TABLE)?;
        tx.open_table(&social_posts_by_time::TABLE)?;
        tx.open_table(&social_posts_reply::TABLE)?;
        Ok(())
    }

    pub(crate) fn handle_db_ver_migrations(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        const DB_VER: u64 = 0;

        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;

        let Some(cur_db_ver) = table_db_ver.first()?.map(|g| g.1.value()) else {
            info!(target: LOG_TARGET, "Initializing new database");
            table_db_ver.insert(&(), &DB_VER)?;

            return Ok(());
        };

        debug!(target: LOG_TARGET, db_ver = cur_db_ver, "Checking db version");
        if DB_VER < cur_db_ver {
            return DbVersionTooHighSnafu {
                db_ver: cur_db_ver,
                code_ver: DB_VER,
            }
            .fail();
        }

        // migration code will go here

        Ok(())
    }

    pub fn read_followees_tx(
        id: RostraId,
        ids_followees_table: &impl ids_followees::ReadableTable,
    ) -> DbResult<HashMap<RostraId, IdsFolloweesRecord>> {
        Ok(ids_followees_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map(|res| res.map(|(k, v)| (k.value().1, v.value())))
            .collect::<Result<HashMap<_, _>, _>>()?)
    }

    pub fn read_followers_tx(
        id: RostraId,
        ids_followers_table: &impl ids_followers::ReadableTable,
    ) -> DbResult<HashMap<RostraId, IdsFollowersRecord>> {
        Ok(ids_followers_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map(|res| res.map(|(k, v)| (k.value().1, v.value())))
            .collect::<Result<HashMap<_, _>, _>>()?)
    }

    pub(crate) fn insert_self_event_id(
        event_id: impl Into<rostra_core::ShortEventId>,
        events_self_table: &mut events_self::Table,
    ) -> DbResult<()> {
        events_self_table.insert(&event_id.into(), &())?;
        Ok(())
    }

    /// Insert an event and do all the accounting for it
    ///
    /// Return `true`
    pub fn insert_event_tx(
        event: VerifiedEvent,
        ids_full_t: &mut ids_full::Table,
        events_table: &mut events::Table,
        events_by_time_table: &mut events_by_time::Table,
        events_content_table: &mut events_content::Table,
        events_missing_table: &mut events_missing::Table,
        events_heads_table: &mut events_heads::Table,
    ) -> DbResult<InsertEventOutcome> {
        let author = event.author();
        let event_id = event.event_id.to_short();

        if events_table.get(&event_id)?.is_some() {
            return Ok(InsertEventOutcome::AlreadyPresent);
        }

        let (id_short, id_rest) = event.author().split();
        ids_full_t.insert(&id_short, &id_rest)?;

        let (was_missing, is_deleted) = if let Some(prev_missing) = events_missing_table
            .remove(&(author, event_id))?
            .map(|g| g.value())
        {
            // if the missing was marked as deleted, we'll record it
            (
                true,
                if let Some(deleted_by) = prev_missing.deleted_by {
                    events_content_table
                        .insert(&event_id, &EventContentState::Deleted { deleted_by })?;
                    true
                } else {
                    false
                },
            )
        } else {
            // since nothing was expecting this event yet, it must be a "head"
            events_heads_table.insert(&(author, event_id), &EventsHeadsTableRecord)?;
            (false, false)
        };

        // When both parents point at same thing, process only one: one that can
        // be responsible for deletion.
        let parent_ids = if event.parent_aux() == event.parent_prev() {
            vec![(event.parent_aux(), true)]
        } else {
            vec![(event.parent_aux(), true), (event.parent_prev(), false)]
        };

        let mut deleted_parent = None;

        let mut deleted_parent_content: Option<EventContent> = None;
        let mut missing_parents = vec![];

        for (parent_id, parent_is_aux) in parent_ids {
            let Some(parent_id) = parent_id else {
                continue;
            };

            let parent_event = events_table.get(&parent_id)?.map(|r| r.value());
            if let Some(_parent_event) = parent_event {
                if event.is_delete_parent_aux_content_set() && parent_is_aux {
                    deleted_parent = Some(parent_id);
                    deleted_parent_content = events_content_table
                        .insert(
                            &parent_id,
                            &EventContentState::Deleted {
                                deleted_by: event_id,
                            },
                        )?
                        .and_then(|deleted_content| match deleted_content.value() {
                            EventContentState::Present(cow) => Some(cow.into_owned()),
                            EventContentState::Deleted { deleted_by: _ } => None,
                            EventContentState::Pruned => None,
                        });
                }
            } else {
                // we do not have this parent yet, so we mark it as missing
                events_missing_table.insert(
                    &(author, parent_id),
                    &EventsMissingRecord {
                        // potentially mark that the missing event was already deleted
                        deleted_by: (event.is_delete_parent_aux_content_set() && parent_is_aux)
                            .then_some(event_id),
                    },
                )?;
                missing_parents.push(parent_id);
            }
            // if the event was considered a "head", it shouldn't as it has a child
            events_heads_table.remove(&(author, parent_id))?;
        }

        events_table.insert(
            &event_id,
            &EventRecord {
                signed: event.into(),
            },
        )?;
        events_by_time_table.insert(&(event.timestamp(), event_id), &())?;

        Ok(InsertEventOutcome::Inserted {
            was_missing,
            is_deleted,
            deleted_parent,
            deleted_parent_content,
            missing_parents,
        }
        .validate())
    }

    #[allow(clippy::needless_lifetimes)]
    pub fn insert_event_content_tx<'t, 'e>(
        VerifiedEventContent { event, content, .. }: &'e VerifiedEventContent,
        events_content_table: &'t mut events_content::Table,
    ) -> DbResult<bool> {
        let event_id = event.event_id.to_short();
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                EventContentState::Deleted { .. } => {
                    return Ok(false);
                }
                EventContentState::Present(_) => {
                    return Ok(false);
                }
                EventContentState::Pruned => {}
            }
        }

        events_content_table.insert(
            &event_id,
            &EventContentState::Present(Cow::Owned(content.clone())),
        )?;

        Ok(true)
    }

    pub fn prune_event_content_tx(
        event_id: impl Into<ShortEventId>,
        events_content_table: &mut events_content::Table,
    ) -> DbResult<bool> {
        let event_id = event_id.into();
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                EventContentState::Deleted { .. } => {
                    return Ok(false);
                }
                EventContentState::Pruned => {
                    return Ok(true);
                }
                EventContentState::Present(_) => {}
            }
        }

        events_content_table.insert(&event_id, &EventContentState::Pruned)?;

        Ok(true)
    }

    pub fn get_missing_events_tx(
        author: RostraId,
        events_missing_table: &impl events_missing::ReadableTable,
    ) -> DbResult<Vec<ShortEventId>> {
        Ok(events_missing_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _v)| k.value().1))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_heads_events_tx(
        author: RostraId,
        events_heads_table: &impl events_heads::ReadableTable,
    ) -> DbResult<Vec<ShortEventId>> {
        Ok(events_heads_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _v)| k.value().1))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_event_tx(
        event: impl Into<ShortEventId>,
        events_table: &impl events::ReadableTable,
    ) -> DbResult<Option<EventRecord>> {
        Ok(events_table.get(&event.into())?.map(|r| r.value()))
    }

    pub fn get_social_post_tx(
        event: impl Into<ShortEventId>,
        social_posts_table: &impl social_posts::ReadableTable,
    ) -> DbResult<Option<SocialPostRecord>> {
        Ok(social_posts_table.get(&event.into())?.map(|r| r.value()))
    }
    pub fn has_event_tx(
        event: impl Into<ShortEventId>,
        events_table: &impl events::ReadableTable,
    ) -> DbResult<bool> {
        Ok(events_table.get(&event.into())?.is_some())
    }

    pub fn get_event_content_tx(
        event: impl Into<ShortEventId>,
        events_content_table: &impl events_content::ReadableTable,
    ) -> DbResult<Option<EventContentState>> {
        Ok(events_content_table.get(&event.into())?.map(|r| r.value()))
    }
    pub fn has_event_content_tx(
        event: impl Into<ShortEventId>,
        events_content_table: &impl events_content::ReadableTable,
    ) -> DbResult<bool> {
        Ok(events_content_table.get(&event.into())?.is_some())
    }

    pub(crate) fn insert_follow_tx(
        author: RostraId,
        timestamp: Timestamp,
        content_kind::Follow { followee, persona }: content_kind::Follow,
        followees_table: &mut Table<(RostraId, RostraId), IdsFolloweesRecord>,
        followers_table: &mut Table<(RostraId, RostraId), IdsFollowersRecord>,
        unfollowed_table: &mut Table<(RostraId, RostraId), IdsUnfollowedRecord>,
    ) -> DbResult<bool> {
        let db_key = (author, followee);
        if let Some(followees) = followees_table.get(&db_key)?.map(|v| v.value()) {
            if timestamp <= followees.ts {
                return Ok(false);
            }
        }
        if let Some(unfollowed) = unfollowed_table.get(&db_key)?.map(|v| v.value()) {
            if timestamp <= unfollowed.ts {
                return Ok(false);
            }
        }

        unfollowed_table.remove(&db_key)?;
        followees_table.insert(
            &db_key,
            &IdsFolloweesRecord {
                ts: timestamp,
                persona,
            },
        )?;
        followers_table.insert(&(followee, author), &IdsFollowersRecord {})?;

        debug!(target: LOG_TARGET, follower = %author.to_short(), followee=%followee.to_short(), "Follow update");

        Ok(true)
    }

    pub(crate) fn insert_unfollow_tx(
        author: RostraId,
        timestamp: Timestamp,
        content_kind::Unfollow { followee }: content_kind::Unfollow,
        followees_table: &mut Table<(RostraId, RostraId), IdsFolloweesRecord>,
        followers_table: &mut Table<(RostraId, RostraId), IdsFollowersRecord>,
        unfollowed_table: &mut Table<(RostraId, RostraId), IdsUnfollowedRecord>,
    ) -> DbResult<bool> {
        let db_key = (author, followee);
        if let Some(followees) = followees_table.get(&db_key)?.map(|v| v.value()) {
            if timestamp <= followees.ts {
                return Ok(false);
            }
        }
        if let Some(unfollowed) = unfollowed_table.get(&db_key)?.map(|v| v.value()) {
            if timestamp <= unfollowed.ts {
                return Ok(false);
            }
        }

        followees_table.remove(&db_key)?;
        followers_table.remove(&(followee, author))?;
        unfollowed_table.insert(&db_key, &IdsUnfollowedRecord { ts: timestamp })?;
        debug!(target: LOG_TARGET, follower = %author.to_short(), followee=%followee.to_short(), "Unfollow update");

        Ok(true)
    }

    pub(crate) fn insert_latest_value_tx<K, V>(
        timestamp: Timestamp,
        key: &K,
        value: V,
        table: &mut redb_bincode::Table<'_, K, Latest<V>>,
    ) -> DbResult<bool>
    where
        K: bincode::Encode + bincode::Decode,
        V: bincode::Encode + bincode::Decode,
    {
        if let Some(existing_value) = table.get(key)?.map(|v| v.value()) {
            if timestamp <= existing_value.ts {
                return Ok(false);
            }
        }

        table.insert(
            key,
            &Latest {
                ts: timestamp,
                inner: value,
            },
        )?;

        Ok(true)
    }

    pub(crate) fn read_self_id_tx(
        id_self_table: &impl ids_self::ReadableTable,
    ) -> Result<Option<IdSelfAccountRecord>, DbError> {
        Ok(id_self_table.get(&())?.map(|v| v.value()))
    }

    pub(crate) fn write_self_id_tx(
        self_id: RostraId,
        id_self_table: &mut Table<(), IdSelfAccountRecord>,
    ) -> DbResult<IdSelfAccountRecord> {
        let id_self_record = IdSelfAccountRecord {
            rostra_id: self_id,
            iroh_secret: thread_rng().gen(),
        };
        let _ = id_self_table.insert(&(), &id_self_record)?;
        Ok(id_self_record)
    }

    pub(crate) fn read_head_tx(
        self_id: RostraId,
        events_heads_table: &impl ReadableTable<(RostraId, ShortEventId), EventsHeadsTableRecord>,
    ) -> DbResult<Option<ShortEventId>> {
        Ok(events_heads_table
            .range((self_id, ShortEventId::ZERO)..=(self_id, ShortEventId::MAX))?
            .next()
            .transpose()?
            .map(|(k, _)| k.value().1))
    }

    pub(crate) fn get_heads_tx(
        self_id: RostraId,
        events_heads_table: &impl events_heads::ReadableTable,
    ) -> DbResult<HashSet<ShortEventId>> {
        Ok(events_heads_table
            .range((self_id, ShortEventId::ZERO)..=(self_id, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _)| k.value().1))
            .collect::<Result<HashSet<_>, _>>()?)
    }

    pub(crate) fn get_social_profile_tx(
        id: RostraId,
        table: &impl social_profiles::ReadableTable,
    ) -> DbResult<Option<IdSocialProfileRecord>> {
        Ok(table.get(&id)?.map(|v| v.value().inner))
    }

    pub(crate) fn get_random_self_event(
        events_self_table: &impl ReadableTable<ShortEventId, ()>,
    ) -> Result<Option<ShortEventId>, DbError> {
        let pivot = ShortEventId::random();

        let before_pivot = (ShortEventId::ZERO)..(pivot);
        let after_pivot = (pivot)..=(ShortEventId::MAX);

        Ok(Some(if thread_rng().gen() {
            if let Some(k) = get_first_in_range(events_self_table, after_pivot)? {
                k
            } else if let Some(k) = get_last_in_range(events_self_table, before_pivot)? {
                k
            } else {
                return Ok(None);
            }
        } else {
            if let Some(k) = get_first_in_range(events_self_table, before_pivot)? {
                k
            } else if let Some(k) = get_last_in_range(events_self_table, after_pivot)? {
                k
            } else {
                return Ok(None);
            }
        }))
    }

    pub fn read_iroh_secret_tx(
        ids_self_t: &impl ids_self::ReadableTable,
    ) -> DbResult<iroh::SecretKey> {
        let self_id = Self::read_self_id_tx(ids_self_t)?
            .expect("Must have iroh secret generated after opening");
        Ok(iroh::SecretKey::from_bytes(&self_id.iroh_secret))
    }
}
