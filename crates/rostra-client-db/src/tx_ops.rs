use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use ids::{IdsFollowersRecord, IdsUnfollowedRecord};
use itertools::Itertools as _;
use rand::Rng as _;
use redb::StorageError;
use redb_bincode::{ReadableTable, Table};
use rostra_core::event::{
    EventContentRaw, EventExt as _, VerifiedEvent, VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ContentHash, ShortEventId, Timestamp};
use tables::EventRecord;
use tables::event::{
    ContentStoreRecord, EventContentResult, EventContentState, EventsMissingRecord,
};
use tables::ids::IdsFolloweesRecord;
use tracing::{debug, error};

use super::id_self::IdSelfAccountRecord;
use super::{
    Database, DbError, DbResult, EventsHeadsTableRecord, InsertEventOutcome, content_rc,
    content_store, events, events_by_time, events_content_state, events_heads, events_missing,
    events_self, get_first_in_range, get_last_in_range, ids, ids_followees, ids_followers,
    ids_self, tables,
};
use crate::{
    IdSocialProfileRecord, IdsDataUsageRecord, LOG_TARGET, Latest, SocialPostRecord, WotData,
    events_content_missing, ids_data_usage, ids_full, social_posts, social_profiles,
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

    /// Compute the web of trust data from the given direct followees.
    ///
    /// The WoT includes:
    /// - Direct followees (passed in)
    /// - Extended followees: followees of direct followees, excluding those
    ///   already in direct followees
    pub fn compute_wot_tx(
        self_id: RostraId,
        direct_followees: &HashMap<RostraId, IdsFolloweesRecord>,
        ids_followees_table: &impl ids_followees::ReadableTable,
    ) -> DbResult<WotData> {
        let mut extended = HashSet::new();

        for followee_id in direct_followees.keys() {
            // Get the followees of this followee
            for result in Self::read_followees_tx_iter(*followee_id, ids_followees_table)? {
                let (ext_id, _record) = result?;
                // Don't include self or direct followees in extended
                if ext_id != self_id && !direct_followees.contains_key(&ext_id) {
                    extended.insert(ext_id);
                }
            }
        }

        Ok(WotData {
            followees: direct_followees.clone(),
            extended,
        })
    }

    pub(crate) fn insert_self_event_id_tx(
        event_id: impl Into<rostra_core::ShortEventId>,
        events_self_table: &mut events_self::Table,
    ) -> DbResult<()> {
        events_self_table.insert(&event_id.into(), &())?;
        Ok(())
    }

    /// Insert an event and perform all DAG accounting.
    ///
    /// This function handles event insertion and related bookkeeping:
    ///
    /// 1. **Identity tracking**: Records the author's full RostraId
    /// 2. **DAG structure**: Updates heads, handles missing parent references
    /// 3. **Content tracking**:
    ///    - Increments RC in `content_rc` for the event's content_hash
    ///    - Marks the event as `Missing` in `events_content_state`
    ///    - If content is not in `content_store`, adds to
    ///      `events_content_missing`
    /// 4. **Deletion handling**: If event is a delete, marks target as deleted
    ///
    /// **Important**: This function does NOT process content side effects (like
    /// incrementing reply counts). That happens in `process_event_content_tx`.
    /// The `Missing` marker ensures content processing is idempotent - it
    /// can be called multiple times for the same event without duplicate
    /// effects.
    ///
    /// Returns [`InsertEventOutcome`] indicating if the event was newly
    /// inserted or already present, along with metadata about the
    /// insertion.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_event_tx(
        event: VerifiedEvent,
        ids_full_t: &mut ids_full::Table,
        events_table: &mut events::Table,
        events_missing_table: &mut events_missing::Table,
        events_heads_table: &mut events_heads::Table,
        events_by_time_table: &mut events_by_time::Table,
        events_content_state_table: &mut events_content_state::Table,
        content_store_table: &mut content_store::Table,
        content_rc_table: &mut content_rc::Table,
        events_content_missing_table: &mut events_content_missing::Table,
        mut ids_data_usage_table: Option<&mut ids_data_usage::Table>,
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
                // If the missing was marked as deleted, we'll record it.
                (
                    true,
                    if let Some(deleted_by) = prev_missing.deleted_by {
                        events_content_state_table
                            .insert(&event_id, &EventContentState::Deleted { deleted_by })?;
                        true
                    } else {
                        false
                    },
                )
            }
            _ => {
                // Since nothing was expecting this event yet, it must be a "head".
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

        let mut reverted_parent_content: Option<EventContentRaw> = None;
        let mut missing_parents = vec![];

        for (parent_id, parent_is_aux) in parent_ids {
            let Some(parent_id) = parent_id else {
                continue;
            };

            let parent_event = events_table.get(&parent_id)?.map(|r| r.value());
            if let Some(parent_event_record) = parent_event {
                if event.is_delete_parent_aux_content_set() && parent_is_aux {
                    deleted_parent = Some(parent_id);
                    events_content_missing_table.remove(&parent_id)?;

                    let parent_content_hash = parent_event_record.content_hash();
                    let old_state = events_content_state_table
                        .insert(
                            &parent_id,
                            &EventContentState::Deleted {
                                deleted_by: event_id,
                            },
                        )?
                        .map(|g| g.value());

                    // Decrement RC unless already decremented
                    // (Deleted/Pruned/Invalid all decrement RC on transition).
                    let rc_already_decremented = matches!(
                        old_state,
                        Some(
                            EventContentState::Deleted { .. }
                                | EventContentState::Pruned
                                | EventContentState::Invalid
                        )
                    );

                    if !rc_already_decremented {
                        // Look up content from content_store to return for
                        // reverting
                        if let Some(ContentStoreRecord(cow)) = content_store_table
                            .get(&parent_content_hash)?
                            .map(|g| g.value())
                        {
                            reverted_parent_content = Some(cow.into_owned());
                        }
                        // Decrement RC for the deleted content
                        Database::decrement_content_rc_tx(parent_content_hash, content_rc_table)?;
                    }

                    // Track payload deletion for the parent's author.
                    // Already-Deleted is skipped (no bucket change), but
                    // Pruned/Invalid → Deleted and Missing/Processed →
                    // Deleted all need tracking.
                    if !matches!(old_state, Some(EventContentState::Deleted { .. })) {
                        if let Some(ref mut usage_table) = ids_data_usage_table {
                            let parent_author = parent_event_record.author();
                            Database::track_payload_deletion_tx(
                                parent_author,
                                parent_event_record.content_len(),
                                old_state.as_ref(),
                                usage_table,
                            )?;
                        }
                    }
                }
            } else {
                // We do not have this parent yet, so we mark it as missing
                events_missing_table.insert(
                    &(author, parent_id),
                    &EventsMissingRecord {
                        // Potentially mark that the missing event was already deleted.
                        deleted_by: (event.is_delete_parent_aux_content_set() && parent_is_aux)
                            .then_some(event_id),
                    },
                )?;
                missing_parents.push(parent_id);
            }
            // If the event was considered a "head", it shouldn't as it has a child.
            events_heads_table.remove(&(author, parent_id))?;
        }

        events_table.insert(
            &event_id,
            &EventRecord {
                signed: event.into(),
            },
        )?;
        events_by_time_table.insert(&(event.timestamp(), event_id), &())?;

        // Track metadata for this event
        if let Some(ref mut usage_table) = ids_data_usage_table {
            Database::track_new_event_tx(author, usage_table)?;
        }

        // Handle content RC and state for this event.
        let content_hash = event.content_hash();
        if !is_deleted {
            // Increment RC for the content hash (including empty content)
            Database::increment_content_rc_tx(content_hash, content_rc_table)?;

            // Track new payload (starts as missing)
            if let Some(ref mut usage_table) = ids_data_usage_table {
                Database::track_new_payload_tx(author, event.content_len(), usage_table)?;
            }

            if 0 < event.content_len() {
                // Regular content: mark as Missing, check content_missing
                events_content_state_table.insert(&event_id, &EventContentState::Missing)?;

                if content_store_table.get(&content_hash)?.is_none() {
                    events_content_missing_table.insert(&event_id, &())?;
                }
            } else {
                // Empty content: store it immediately, go straight to "processed"
                if content_store_table.get(&content_hash)?.is_none() {
                    content_store_table.insert(
                        &content_hash,
                        &ContentStoreRecord(Cow::Owned(EventContentRaw::new(vec![]))),
                    )?;
                }
                // Move from missing to current (was tracked as missing above)
                if let Some(ref mut usage_table) = ids_data_usage_table {
                    Database::track_payload_processed_tx(author, event.content_len(), usage_table)?;
                }
            }
        }

        Ok(InsertEventOutcome::Inserted {
            was_missing,
            is_deleted,
            deleted_parent,
            reverted_parent_content,
            missing_parents,
        }
        .validate())
    }

    /// Check if we should process content for this event.
    ///
    /// This function ensures idempotent content processing by checking the
    /// event's state in `events_content_state`:
    ///
    /// - **`Missing`** → `true`: Event was inserted but content hasn't been
    ///   processed yet. Side effects should be applied.
    /// - **No entry** → `false`: Content was already processed. Returning false
    ///   prevents duplicate side effects (e.g., incrementing reply_count
    ///   twice).
    /// - **`Deleted`/`Pruned`** → `false`: Content is unwanted.
    ///
    /// After processing content, callers should remove the `Missing` marker
    /// from `events_content_state` to indicate processing is complete.
    pub fn can_insert_event_content_tx(
        VerifiedEventContent { event, .. }: &VerifiedEventContent,
        events_content_state_table: &impl events_content_state::ReadableTable,
    ) -> DbResult<bool> {
        let event_id = event.event_id.to_short();

        // Check content state
        if let Some(state) = events_content_state_table
            .get(&event_id)?
            .map(|g| g.value())
        {
            match state {
                EventContentState::Missing => {
                    // Content not yet processed, can insert
                    return Ok(true);
                }
                EventContentState::Deleted { .. }
                | EventContentState::Pruned
                | EventContentState::Invalid => {
                    // Content deleted, pruned, or invalid — cannot insert
                    return Ok(false);
                }
            }
        }

        // No state means content was already processed (came with event or processed
        // earlier) Return false to skip reprocessing
        Ok(false)
    }

    /// Mark an event's content as pruned.
    ///
    /// In the new model, RC was incremented when the event was inserted,
    /// so we decrement it here (unless already deleted/pruned).
    pub fn prune_event_content_tx(
        event_id: impl Into<ShortEventId>,
        content_hash: ContentHash,
        events_content_state_table: &mut events_content_state::Table,
        content_rc_table: &mut content_rc::Table,
        events_content_missing_table: &mut events_content_missing::Table,
        data_usage_info: Option<(RostraId, u32, &mut ids_data_usage::Table)>,
    ) -> DbResult<bool> {
        let event_id = event_id.into();

        // Check current state - if already deleted or pruned, handle appropriately
        let old_state = events_content_state_table
            .get(&event_id)?
            .map(|g| g.value());

        match old_state {
            Some(EventContentState::Deleted { .. }) => {
                // Already deleted, can't prune
                return Ok(false);
            }
            Some(EventContentState::Invalid) => {
                // Already invalid (RC already decremented), can't prune
                return Ok(false);
            }
            Some(EventContentState::Pruned) => {
                // Already pruned, nothing to do
                return Ok(true);
            }
            Some(EventContentState::Missing) | None => {
                // Can proceed to prune
            }
        }

        // Not yet deleted/pruned - decrement RC and mark as pruned
        Database::decrement_content_rc_tx(content_hash, content_rc_table)?;

        // Track payload pruning
        if let Some((author, content_len, usage_table)) = data_usage_info {
            Database::track_payload_pruning_tx(
                author,
                content_len,
                old_state.as_ref(),
                usage_table,
            )?;
        }

        events_content_state_table.insert(&event_id, &EventContentState::Pruned)?;
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

    pub fn count_missing_events_for_id_tx(
        author: RostraId,
        events_missing_table: &impl events_missing::ReadableTable,
    ) -> DbResult<usize> {
        Ok(events_missing_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .count())
    }

    pub fn count_heads_events_tx(
        author: RostraId,
        events_heads_table: &impl events_heads::ReadableTable,
    ) -> DbResult<usize> {
        Ok(events_heads_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .count())
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

    /// Get the per-event content state (not the content itself).
    ///
    /// To get the actual content, use `get_event_content_full_tx` which also
    /// looks up the content from the content_store.
    pub fn get_event_content_state_tx(
        event: impl Into<ShortEventId>,
        events_content_state_table: &impl events_content_state::ReadableTable,
    ) -> DbResult<Option<EventContentState>> {
        Ok(events_content_state_table
            .get(&event.into())?
            .map(|r| r.value()))
    }

    /// Get the full content for an event, including looking it up from
    /// content_store.
    ///
    /// Returns:
    /// - `None` if no state recorded for this event
    /// - `Some(Present(content))` if content is available
    /// - `Some(Invalid(content))` if content was invalid
    /// - `Some(Deleted { deleted_by })` if content was deleted
    /// - `Some(Pruned)` if content was pruned
    /// - `Some(Missing)` if content is not in store
    pub fn get_event_content_full_tx(
        event_id: impl Into<ShortEventId>,
        content_hash: ContentHash,
        events_content_state_table: &impl events_content_state::ReadableTable,
        content_store_table: &impl content_store::ReadableTable,
    ) -> DbResult<Option<EventContentResult>> {
        let event_id = event_id.into();

        // Check if content is deleted or pruned
        // Check if deleted or pruned - return corresponding result
        if let Some(state) = events_content_state_table
            .get(&event_id)?
            .map(|r| r.value())
        {
            match state {
                EventContentState::Missing => {
                    // Content not yet processed - fall through to check
                    // content_store
                }
                EventContentState::Deleted { deleted_by } => {
                    return Ok(Some(EventContentResult::Deleted { deleted_by }));
                }
                EventContentState::Pruned => {
                    return Ok(Some(EventContentResult::Pruned));
                }
                EventContentState::Invalid => {
                    return Ok(Some(EventContentResult::Invalid));
                }
            }
        }

        // Not deleted/pruned/invalid - look up content from content_store
        Ok(Some(
            match content_store_table.get(&content_hash)?.map(|r| r.value()) {
                Some(ContentStoreRecord(content)) => {
                    EventContentResult::Present(content.into_owned())
                }
                None => EventContentResult::Missing,
            },
        ))
    }

    /// Check if content is available for an event.
    ///
    /// In the new model, content is available if:
    /// - Event is NOT in deleted/pruned state, AND
    /// - Content hash is in content_store
    pub fn has_event_content_tx(
        event_id: impl Into<ShortEventId>,
        content_hash: ContentHash,
        events_content_state_table: &impl events_content_state::ReadableTable,
        content_store_table: &impl content_store::ReadableTable,
    ) -> DbResult<bool> {
        let event_id = event_id.into();

        // If event has a content state, it means content is deleted or pruned
        if events_content_state_table.get(&event_id)?.is_some() {
            return Ok(false);
        }

        // Check if content is in store
        Ok(content_store_table.get(&content_hash)?.is_some())
    }

    /// Check if content is available for an event that was marked as missing.
    ///
    /// This is for cases where:
    /// - Event A was inserted but its content wasn't available yet
    /// - Later, content arrived via event B (which has the same content hash)
    /// - Now we want to check if event A can use that content
    ///
    /// In the new model, RC is managed at event insertion time, so this
    /// function doesn't touch RC. It just checks if content is available.
    ///
    /// Returns `true` if content is in store and event is not deleted/pruned.
    pub fn is_content_available_for_event_tx(
        event_id: impl Into<ShortEventId>,
        content_hash: ContentHash,
        events_content_state_table: &impl events_content_state::ReadableTable,
        content_store_table: &impl content_store::ReadableTable,
    ) -> DbResult<bool> {
        let event_id = event_id.into();

        // Check event content state
        if let Some(state) = events_content_state_table
            .get(&event_id)?
            .map(|g| g.value())
        {
            match state {
                // Missing means content wasn't in store at event insertion,
                // but it might be now - check the store
                EventContentState::Missing => {}
                // Deleted, pruned, or invalid means content is not available
                EventContentState::Deleted { .. }
                | EventContentState::Pruned
                | EventContentState::Invalid => {
                    return Ok(false);
                }
            }
        }

        // Check if content is in store
        Ok(content_store_table.get(&content_hash)?.is_some())
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
        followee: RostraId,
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
        K: bincode::Encode + bincode::Decode<()>,
        V: bincode::Encode + bincode::Decode<()>,
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
            iroh_secret: rand::rng().random(),
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

        Ok(Some(if rand::rng().random() {
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

    /// Increment reference count for content by its hash.
    ///
    /// Called when a new event referencing this content is inserted.
    pub fn increment_content_rc_tx(
        content_hash: ContentHash,
        content_rc_table: &mut content_rc::Table,
    ) -> DbResult<u64> {
        let current_count = content_rc_table
            .get(&content_hash)?
            .map(|g| g.value())
            .unwrap_or(0); // Default to 0 if missing (first reference)

        let new_count = current_count + 1;
        content_rc_table.insert(&content_hash, &new_count)?;
        Ok(new_count)
    }

    /// Decrement reference count for content by its hash.
    ///
    /// Called when an event's content is deleted or pruned.
    /// Note: This does NOT remove the content from content_store - that should
    /// be done separately via garbage collection when RC reaches 0.
    pub fn decrement_content_rc_tx(
        content_hash: ContentHash,
        content_rc_table: &mut content_rc::Table,
    ) -> DbResult<u64> {
        let current_count = match content_rc_table.get(&content_hash)?.map(|g| g.value()) {
            Some(count) => count,
            None => {
                // RC entry missing - this shouldn't happen in normal operation.
                // It means decrement was called without a corresponding increment.
                debug_assert!(
                    false,
                    "Decrementing RC for content with no RC entry: {content_hash}"
                );
                error!(
                    target: LOG_TARGET,
                    %content_hash,
                    "Decrementing RC for content with no RC entry - possible bug"
                );
                // Default to 1 to avoid underflow, will result in RC=0
                1
            }
        };

        if current_count <= 1 {
            // Count reached 0, remove the RC entry
            // (content_store cleanup is separate)
            content_rc_table.remove(&content_hash)?;
            Ok(0)
        } else {
            let new_count = current_count
                .checked_sub(1)
                .expect("Reference count should never underflow");
            content_rc_table.insert(&content_hash, &new_count)?;
            Ok(new_count)
        }
    }

    /// Get the reference count for content by its hash.
    pub fn get_content_rc_tx(
        content_hash: ContentHash,
        content_rc_table: &impl content_rc::ReadableTable,
    ) -> DbResult<u64> {
        Ok(content_rc_table
            .get(&content_hash)?
            .map(|g| g.value())
            .unwrap_or(0)) // Default to 0 if missing
    }

    /// Remove an event and handle reference counting for its content.
    ///
    /// This removes the event from the events table and decrements the
    /// reference count for its content hash.
    pub fn remove_event_tx(
        event_id: ShortEventId,
        events_table: &mut events::Table,
        events_content_state_table: &mut events_content_state::Table,
        content_rc_table: &mut content_rc::Table,
        events_content_missing_table: &mut events_content_missing::Table,
    ) -> DbResult<bool> {
        let event = events_table.remove(&event_id)?.map(|g| g.value());

        if let Some(event_record) = event {
            let content_hash = event_record.content_hash();

            // Check if the event's content was already deleted/pruned
            // In the new model, RC was incremented at insertion time,
            // so we decrement here unless it was already decremented (deleted/pruned)
            let was_already_unwanted = events_content_state_table
                .get(&event_id)?
                .map(|g| {
                    matches!(
                        g.value(),
                        EventContentState::Deleted { .. } | EventContentState::Pruned
                    )
                })
                .unwrap_or(false);

            // Remove per-event state
            events_content_state_table.remove(&event_id)?;
            events_content_missing_table.remove(&event_id)?;

            // Decrement RC if content wasn't already unwanted
            if !was_already_unwanted {
                Database::decrement_content_rc_tx(content_hash, content_rc_table)?;
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }

    // ========================================================================
    // Data Usage Tracking
    // ========================================================================

    /// Size of event metadata in bytes (Event struct + signature).
    /// See rostra_core::event::Event documentation.
    pub const EVENT_METADATA_SIZE: u64 = 192;

    fn get_usage_mut(
        author: RostraId,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<IdsDataUsageRecord> {
        Ok(ids_data_usage_table
            .get(&author)?
            .map(|g| g.value())
            .unwrap_or_default())
    }

    /// Track a newly inserted event (metadata only).
    ///
    /// Called once per event in `insert_event_tx`.
    pub fn track_new_event_tx(
        author: RostraId,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<()> {
        let mut usage = Self::get_usage_mut(author, ids_data_usage_table)?;

        usage.current_metadata_size += Self::EVENT_METADATA_SIZE;
        usage.total_metadata_size += Self::EVENT_METADATA_SIZE;
        usage.current_metadata_num += 1;
        usage.total_metadata_num += 1;

        ids_data_usage_table.insert(&author, &usage)?;
        Ok(())
    }

    /// Track a newly inserted payload (starts as missing).
    ///
    /// Called in `insert_event_tx` when an event with `content_len > 0` is
    /// inserted and not deleted. The payload is counted in total and missing
    /// until content is received and processed.
    pub fn track_new_payload_tx(
        author: RostraId,
        content_len: u32,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<()> {
        let len = u64::from(content_len);
        let mut usage = Self::get_usage_mut(author, ids_data_usage_table)?;

        usage.total_content_size += len;
        usage.total_payload_num += 1;
        usage.missing_payload_size += len;
        usage.missing_payload_num += 1;

        ids_data_usage_table.insert(&author, &usage)?;
        Ok(())
    }

    /// Track a payload that has been processed (missing → current).
    ///
    /// Called in `process_event_content_tx` when content transitions from
    /// `Missing` to processed.
    pub fn track_payload_processed_tx(
        author: RostraId,
        content_len: u32,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<()> {
        let len = u64::from(content_len);
        let mut usage = Self::get_usage_mut(author, ids_data_usage_table)?;

        usage.missing_payload_size = usage.missing_payload_size.saturating_sub(len);
        usage.missing_payload_num = usage.missing_payload_num.saturating_sub(1);
        usage.current_content_size += len;
        usage.current_payload_num += 1;

        ids_data_usage_table.insert(&author, &usage)?;
        Ok(())
    }

    /// Track a payload that failed validation (missing → invalid).
    ///
    /// Called in `process_event_content_tx` when content fails deserialization.
    pub fn track_payload_invalid_tx(
        author: RostraId,
        content_len: u32,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<()> {
        let len = u64::from(content_len);
        let mut usage = Self::get_usage_mut(author, ids_data_usage_table)?;

        usage.missing_payload_size = usage.missing_payload_size.saturating_sub(len);
        usage.missing_payload_num = usage.missing_payload_num.saturating_sub(1);
        usage.invalid_payload_size += len;
        usage.invalid_payload_num += 1;

        ids_data_usage_table.insert(&author, &usage)?;
        Ok(())
    }

    /// Track a payload deletion (missing/current/invalid/pruned → deleted).
    ///
    /// `old_state` determines which bucket the payload moves from:
    /// - `Some(Missing)` → moves from missing to deleted
    /// - `Some(Invalid)` → moves from invalid to deleted
    /// - `Some(Pruned)` → moves from pruned to deleted
    /// - `None` (processed) → moves from current to deleted
    pub fn track_payload_deletion_tx(
        author: RostraId,
        content_len: u32,
        old_state: Option<&EventContentState>,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<()> {
        let len = u64::from(content_len);
        let mut usage = Self::get_usage_mut(author, ids_data_usage_table)?;

        match old_state {
            Some(EventContentState::Missing) => {
                usage.missing_payload_size = usage.missing_payload_size.saturating_sub(len);
                usage.missing_payload_num = usage.missing_payload_num.saturating_sub(1);
            }
            Some(EventContentState::Invalid) => {
                usage.invalid_payload_size = usage.invalid_payload_size.saturating_sub(len);
                usage.invalid_payload_num = usage.invalid_payload_num.saturating_sub(1);
            }
            Some(EventContentState::Pruned) => {
                usage.pruned_payload_size = usage.pruned_payload_size.saturating_sub(len);
                usage.pruned_payload_num = usage.pruned_payload_num.saturating_sub(1);
            }
            None => {
                usage.current_content_size = usage.current_content_size.saturating_sub(len);
                usage.current_payload_num = usage.current_payload_num.saturating_sub(1);
            }
            // Already deleted — should not happen (caller guards against it)
            Some(EventContentState::Deleted { .. }) => {}
        }

        usage.deleted_payload_size += len;
        usage.deleted_payload_num += 1;

        ids_data_usage_table.insert(&author, &usage)?;
        Ok(())
    }

    /// Track a payload pruning (missing or current → pruned).
    ///
    /// `old_state` determines which bucket the payload moves from:
    /// - `Some(Missing)` → moves from missing to pruned
    /// - `None` (processed) → moves from current to pruned
    pub fn track_payload_pruning_tx(
        author: RostraId,
        content_len: u32,
        old_state: Option<&EventContentState>,
        ids_data_usage_table: &mut ids_data_usage::Table,
    ) -> DbResult<()> {
        let len = u64::from(content_len);
        let mut usage = Self::get_usage_mut(author, ids_data_usage_table)?;

        match old_state {
            Some(EventContentState::Missing) => {
                usage.missing_payload_size = usage.missing_payload_size.saturating_sub(len);
                usage.missing_payload_num = usage.missing_payload_num.saturating_sub(1);
            }
            None => {
                usage.current_content_size = usage.current_content_size.saturating_sub(len);
                usage.current_payload_num = usage.current_payload_num.saturating_sub(1);
            }
            _ => {}
        }

        usage.pruned_payload_size += len;
        usage.pruned_payload_num += 1;

        ids_data_usage_table.insert(&author, &usage)?;
        Ok(())
    }

    /// Get the data usage for an identity.
    pub fn get_data_usage_tx(
        author: RostraId,
        ids_data_usage_table: &impl ids_data_usage::ReadableTable,
    ) -> DbResult<IdsDataUsageRecord> {
        Ok(ids_data_usage_table
            .get(&author)?
            .map(|g| g.value())
            .unwrap_or_default())
    }
}
