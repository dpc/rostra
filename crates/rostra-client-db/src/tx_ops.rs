use std::collections::{HashMap, HashSet};

use ids::{IdsFollowersRecord, IdsUnfollowedRecord};
use itertools::Itertools as _;
use rand::{Rng as _, thread_rng};
use redb::StorageError;
use redb_bincode::{ReadableTable, Table};
use rostra_core::event::{
    EventContent, EventExt as _, VerifiedEvent, VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use tables::EventRecord;
use tables::event::{EventContentState, EventsMissingRecord};
use tables::ids::IdsFolloweesRecord;
use tracing::debug;

use super::id_self::IdSelfAccountRecord;
use super::{
    Database, DbError, DbResult, EventsHeadsTableRecord, InsertEventOutcome, events,
    events_by_time, events_content, events_heads, events_missing, events_self, get_first_in_range,
    get_last_in_range, ids, ids_followees, ids_followers, ids_self, tables,
};
use crate::{
    IdSocialProfileRecord, LOG_TARGET, Latest, SocialPostRecord, events_content_missing, ids_full,
    social_posts, social_profiles,
};

impl Database {
    pub fn read_followees_tx(
        id: RostraId,
        ids_followees_table: &impl ids_followees::ReadableTable,
    ) -> DbResult<HashMap<RostraId, IdsFolloweesRecord>> {
        Ok(ids_followees_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map(|res| res.map(|(k, v)| (k.value().1, v.value())))
            .collect::<Result<HashMap<_, _>, _>>()?)
    }
    pub fn read_followees_tx_iter(
        id: RostraId,
        ids_followees_table: &impl ids_followees::ReadableTable,
    ) -> DbResult<impl Iterator<Item = Result<(RostraId, IdsFolloweesRecord), StorageError>>> {
        Ok(ids_followees_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map_ok(|(k, v)| (k.value().1, v.value())))
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

    pub(crate) fn insert_self_event_id_tx(
        event_id: impl Into<rostra_core::ShortEventId>,
        events_self_table: &mut events_self::Table,
    ) -> DbResult<()> {
        events_self_table.insert(&event_id.into(), &())?;
        Ok(())
    }

    /// Insert an event and do all the accounting for it
    ///
    /// Return `true`
    #[allow(clippy::too_many_arguments)]
    pub fn insert_event_tx(
        event: VerifiedEvent,
        ids_full_t: &mut ids_full::Table,
        events_table: &mut events::Table,
        events_missing_table: &mut events_missing::Table,
        events_heads_table: &mut events_heads::Table,
        events_by_time_table: &mut events_by_time::Table,
        events_content_table: &mut events_content::Table,
        events_content_missing_table: &mut events_content_missing::Table,
    ) -> DbResult<InsertEventOutcome> {
        let author = event.author();
        let event_id = event.event_id.to_short();

        if events_table.get(&event_id)?.is_some() {
            return Ok(InsertEventOutcome::AlreadyPresent);
        }

        let (id_short, id_rest) = event.author().split();
        ids_full_t.insert(&id_short, &id_rest)?;

        let (was_missing, is_deleted) = match events_missing_table
            .remove(&(author, event_id))?
            .map(|g| g.value())
        {
            Some(prev_missing) => {
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
            }
            _ => {
                // since nothing was expecting this event yet, it must be a "head"
                events_heads_table.insert(&(author, event_id), &EventsHeadsTableRecord)?;
                (false, false)
            }
        };

        // When both parents point at same thing, process only one: one that can
        // be responsible for deletion.
        let parent_ids = if event.parent_aux() == event.parent_prev() {
            vec![(event.parent_aux(), true)]
        } else {
            vec![(event.parent_aux(), true), (event.parent_prev(), false)]
        };

        let mut deleted_parent = None;

        let mut reverted_parent_content: Option<EventContent> = None;
        let mut missing_parents = vec![];

        for (parent_id, parent_is_aux) in parent_ids {
            let Some(parent_id) = parent_id else {
                continue;
            };

            let parent_event = events_table.get(&parent_id)?.map(|r| r.value());
            if let Some(_parent_event) = parent_event {
                if event.is_delete_parent_aux_content_set() && parent_is_aux {
                    deleted_parent = Some(parent_id);
                    events_content_missing_table.remove(&parent_id)?;
                    reverted_parent_content = events_content_table
                        .insert(
                            &parent_id,
                            &EventContentState::Deleted {
                                deleted_by: event_id,
                            },
                        )?
                        .and_then(|deleted_content| match deleted_content.value() {
                            EventContentState::Present(cow) => Some(cow.into_owned()),
                            EventContentState::Deleted { deleted_by: _ } => None,
                            EventContentState::Pruned
                            |
                            // There is no need to revert this event, so we don't return it
                            EventContentState::Invalid(_) => None,
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
            reverted_parent_content,
            missing_parents,
        }
        .validate())
    }

    pub fn can_insert_event_content_tx(
        VerifiedEventContent { event, .. }: &VerifiedEventContent,
        events_content_table: &mut events_content::Table,
    ) -> DbResult<bool> {
        let event_id = event.event_id.to_short();
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                EventContentState::Deleted { .. } => {
                    return Ok(false);
                }
                EventContentState::Present(_) | EventContentState::Invalid(_) => {
                    return Ok(false);
                }
                EventContentState::Pruned => {}
            }
        }

        Ok(true)
    }

    pub fn prune_event_content_tx(
        event_id: impl Into<ShortEventId>,
        events_content_table: &mut events_content::Table,
        events_content_missing_table: &mut events_content_missing::Table,
    ) -> DbResult<bool> {
        let event_id = event_id.into();
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                EventContentState::Deleted { .. } => {
                    return Ok(false);
                }
                EventContentState::Pruned => {
                    // already pruned, no need to do anything
                    return Ok(true);
                }
                EventContentState::Invalid(_) | EventContentState::Present(_) => {
                    // go ahead and mark as pruned
                }
            }
        }

        events_content_table.insert(&event_id, &EventContentState::Pruned)?;
        events_content_missing_table.remove(&event_id)?;

        Ok(true)
    }

    pub fn get_missing_events_for_id_tx(
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
        content: content_kind::Follow,
        followees_table: &mut Table<(RostraId, RostraId), IdsFolloweesRecord>,
        followers_table: &mut Table<(RostraId, RostraId), IdsFollowersRecord>,
        unfollowed_table: &mut Table<(RostraId, RostraId), IdsUnfollowedRecord>,
    ) -> DbResult<bool> {
        let followee = content.followee;
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

        let selector = content.selector();
        if selector.is_some() {
            unfollowed_table.remove(&db_key)?;
        }
        followees_table.insert(
            &db_key,
            &IdsFolloweesRecord {
                ts: timestamp,
                selector,
            },
        )?;
        followers_table.insert(&(followee, author), &IdsFollowersRecord {})?;

        debug!(target: LOG_TARGET, follower = %author.to_short(), followee=%followee.to_short(), "Follow update");

        Ok(true)
    }

    #[allow(deprecated)]
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
            iroh_secret: thread_rng().r#gen(),
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

        Ok(Some(if thread_rng().r#gen() {
            match get_first_in_range(events_self_table, after_pivot)? {
                Some(k) => k,
                _ => match get_last_in_range(events_self_table, before_pivot)? {
                    Some(k) => k,
                    _ => {
                        return Ok(None);
                    }
                },
            }
        } else {
            match get_first_in_range(events_self_table, before_pivot)? {
                Some(k) => k,
                _ => match get_last_in_range(events_self_table, after_pivot)? {
                    Some(k) => k,
                    _ => {
                        return Ok(None);
                    }
                },
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
