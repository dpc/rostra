use std::borrow::{Borrow as _, Cow};

use events::ContentStateRef;
use ids::{IdsFollowersRecord, IdsUnfollowedRecord};
use rand::{thread_rng, Rng as _};
use redb_bincode::{ReadableTable, Table};
use rostra_core::event::{content, SignedEvent, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use tables::events::EventsMissingRecord;
use tables::ids::IdsFolloweesRecord;
use tables::{ContentState, EventRecord, EventsHeadsTableValue};
use tracing::debug;

use super::{
    events, get_first_in_range, get_last_in_range, ids, tables, Database, DbError, DbResult,
    InsertEventOutcome,
};
use crate::db::LOG_TARGET;

impl Database {
    pub fn read_followees_tx(
        id: RostraId,
        ids_followees_table: &impl ReadableTable<(RostraId, RostraId), IdsFolloweesRecord>,
    ) -> DbResult<Vec<(RostraId, IdsFolloweesRecord)>> {
        Ok(ids_followees_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map(|res| res.map(|(k, v)| (k.value().1, v.value())))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn read_followers_tx(
        id: RostraId,
        ids_followers_table: &impl ReadableTable<(RostraId, RostraId), IdsFollowersRecord>,
    ) -> DbResult<Vec<(RostraId, IdsFollowersRecord)>> {
        Ok(ids_followers_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map(|res| res.map(|(k, v)| (k.value().1, v.value())))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn insert_self_event_id(
        event_id: impl Into<rostra_core::ShortEventId>,

        events_self_table: &mut Table<ShortEventId, ()>,
    ) -> DbResult<()> {
        events_self_table.insert(&event_id.into(), &())?;
        Ok(())
    }

    /// Insert an event and do all the accounting for it
    ///
    /// Return `true`
    pub fn insert_event_tx(
        VerifiedEvent {
            event_id,
            event,
            sig,
        }: &VerifiedEvent,
        events_table: &mut Table<ShortEventId, EventRecord>,
        events_by_time_table: &mut Table<(Timestamp, ShortEventId), ()>,
        events_content_table: &mut Table<ShortEventId, ContentState>,
        events_missing_table: &mut Table<(RostraId, ShortEventId), EventsMissingRecord>,
        events_heads_table: &mut Table<(RostraId, ShortEventId), EventsHeadsTableValue>,
    ) -> DbResult<InsertEventOutcome> {
        let author = event.author;
        let event_id = ShortEventId::from(*event_id);

        if events_table.get(&event_id)?.is_some() {
            return Ok(InsertEventOutcome::AlreadyPresent);
        }

        let (was_missing, is_deleted) = if let Some(prev_missing) = events_missing_table
            .remove(&(author, event_id))?
            .map(|g| g.value())
        {
            // if the missing was marked as deleted, we'll record it
            (
                true,
                if let Some(deleted_by) = prev_missing.deleted_by {
                    events_content_table
                        .insert(&event_id, &ContentState::Deleted { deleted_by })?;
                    true
                } else {
                    false
                },
            )
        } else {
            // since nothing was expecting this event yet, it must be a "head"
            events_heads_table.insert(&(author, event_id), &EventsHeadsTableValue)?;
            (false, false)
        };

        // When both parents point at same thing, process only one: one that can
        // be responsible for deletion.
        let parent_ids = if event.parent_aux == event.parent_prev {
            vec![(event.parent_aux, true)]
        } else {
            vec![(event.parent_aux, true), (event.parent_prev, false)]
        };

        let mut deleted_parent = None;
        let mut missing_parents = vec![];

        for (parent_id, parent_is_aux) in parent_ids {
            let Some(parent_id) = parent_id.into() else {
                continue;
            };

            let parent_event = events_table.get(&parent_id)?.map(|r| r.value());
            if let Some(_parent_event) = parent_event {
                if event.is_delete_parent_aux_content_set() && parent_is_aux {
                    deleted_parent = Some(parent_id);
                    events_content_table.insert(
                        &parent_id,
                        &ContentState::Deleted {
                            deleted_by: event_id,
                        },
                    )?;
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
                event: SignedEvent {
                    event: *event,
                    sig: *sig,
                },
            },
        )?;
        events_by_time_table.insert(&(event.timestamp.into(), event_id), &())?;

        Ok(InsertEventOutcome::Inserted {
            was_missing,
            is_deleted,
            deleted_parent_content: deleted_parent,
            missing_parents,
        })
    }

    #[allow(clippy::needless_lifetimes)]
    pub fn insert_event_content_tx<'t, 'e>(
        VerifiedEventContent {
            event_id, content, ..
        }: &'e VerifiedEventContent,
        events_content_table: &'t mut Table<ShortEventId, ContentState>,
    ) -> DbResult<bool> {
        let event_id = event_id.to_short();
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                ContentState::Deleted { .. } => {
                    return Ok(false);
                }
                ContentState::Present(_) => {
                    return Ok(false);
                }
                ContentState::Pruned => {}
            }
        }

        let borrow = content.borrow();
        let borrowed: Cow<'_, rostra_core::event::EventContentData> = Cow::Borrowed(borrow);
        events_content_table.insert(&event_id, &ContentStateRef::Present(borrowed))?;

        Ok(true)
    }

    pub fn prune_event_content_tx(
        event_id: impl Into<ShortEventId>,
        events_content_table: &mut Table<ShortEventId, ContentState>,
    ) -> DbResult<bool> {
        let event_id = event_id.into();
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                ContentState::Deleted { .. } => {
                    return Ok(false);
                }
                ContentState::Pruned => {
                    return Ok(true);
                }
                ContentState::Present(_) => {}
            }
        }

        events_content_table.insert(&event_id, &ContentState::Pruned)?;

        Ok(true)
    }

    pub fn get_missing_events_tx(
        author: RostraId,
        events_missing_table: &impl ReadableTable<(RostraId, ShortEventId), EventsMissingRecord>,
    ) -> DbResult<Vec<ShortEventId>> {
        Ok(events_missing_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _v)| k.value().1))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_heads_events_tx(
        author: RostraId,
        events_heads_table: &impl ReadableTable<(RostraId, ShortEventId), EventsHeadsTableValue>,
    ) -> DbResult<Vec<ShortEventId>> {
        Ok(events_heads_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _v)| k.value().1))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_event_tx(
        event: impl Into<ShortEventId>,
        events_table: &impl ReadableTable<ShortEventId, EventRecord>,
    ) -> DbResult<Option<EventRecord>> {
        Ok(events_table.get(&event.into())?.map(|r| r.value()))
    }

    pub fn has_event_tx(
        event: impl Into<ShortEventId>,
        events_table: &impl ReadableTable<ShortEventId, EventRecord>,
    ) -> DbResult<bool> {
        Ok(events_table.get(&event.into())?.is_some())
    }

    pub fn get_event_content_tx(
        event: impl Into<ShortEventId>,
        events_content_table: &impl ReadableTable<ShortEventId, ContentState>,
    ) -> DbResult<Option<ContentState>> {
        Ok(events_content_table.get(&event.into())?.map(|r| r.value()))
    }
    pub fn has_event_content_tx(
        event: impl Into<ShortEventId>,
        events_content_table: &impl ReadableTable<ShortEventId, ContentState>,
    ) -> DbResult<bool> {
        Ok(events_content_table.get(&event.into())?.is_some())
    }

    pub(crate) fn insert_follow_tx(
        author: RostraId,
        timestamp: Timestamp,
        content::Follow { followee, persona }: content::Follow,

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
        content::Unfollow { followee }: content::Unfollow,
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

    pub(crate) fn read_self_id_tx(
        id_self_table: &impl ReadableTable<(), RostraId>,
    ) -> Result<Option<RostraId>, DbError> {
        Ok(id_self_table.get(&())?.map(|v| v.value()))
    }

    pub(crate) fn write_self_id_tx(
        self_id: RostraId,
        id_self_table: &mut Table<(), RostraId>,
    ) -> DbResult<()> {
        let _ = id_self_table.insert(&(), &self_id)?;
        Ok(())
    }

    pub(crate) fn get_head_tx(
        self_id: RostraId,
        events_heads_table: &impl ReadableTable<(RostraId, ShortEventId), EventsHeadsTableValue>,
    ) -> DbResult<Option<ShortEventId>> {
        Ok(events_heads_table
            .range((self_id, ShortEventId::ZERO)..=(self_id, ShortEventId::MAX))?
            .next()
            .transpose()?
            .map(|(k, _)| k.value().1))
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
}
