mod models;
mod tables;

use std::borrow::{Borrow as _, Cow};
use std::ops;
use std::path::PathBuf;

use events::ContentStateRef;
use ids::{IdsFollowersRecord, IdsUnfollowedRecord};
use rand::{thread_rng, Rng as _};
use redb_bincode::{ReadTransaction, ReadableTable, Table, WriteTransaction};
use rostra_core::event::{
    ContentFollow, ContentUnfollow, SignedEvent, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::BoxedError;
use snafu::{Location, ResultExt as _, Snafu};
use tables::events::EventsMissingRecord;
use tables::ids::IdsFolloweesRecord;
use tables::{ContentState, EventRecord, EventsHeadsTableValue};
use tokio::task::JoinError;
use tracing::{debug, info, instrument};

pub use self::tables::*;

const LOG_TARGET: &str = "rostra::db";

#[derive(Debug, Snafu)]
pub enum DbError {
    Database {
        source: redb::DatabaseError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    Table {
        source: redb::TableError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    Storage {
        source: redb::StorageError,
        #[snafu(implicit)]
        location: Location,
    },
    Transaction {
        source: redb::TransactionError,
        #[snafu(implicit)]
        location: Location,
    },
    Commit {
        source: redb::CommitError,
        #[snafu(implicit)]
        location: Location,
    },
    DbVersionTooHigh {
        db_ver: u64,
        code_ver: u64,
        #[snafu(implicit)]
        location: Location,
    },
    Join {
        source: JoinError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    DbTxLogic {
        source: BoxedError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(visibility(pub))]
    IdMismatch {
        #[snafu(implicit)]
        location: Location,
    },
}
pub type DbResult<T> = std::result::Result<T, DbError>;

#[derive(Debug)]
pub struct Database(redb_bincode::Database);

impl Database {
    const DB_VER: u64 = 0;

    #[instrument(skip_all)]
    pub async fn open(path: impl Into<PathBuf>, self_id: RostraId) -> DbResult<Database> {
        let path = path.into();
        debug!(target: LOG_TARGET, path = %path.display(), "Opening database");
        let create = tokio::task::spawn_blocking(move || redb_bincode::Database::create(path))
            .await
            .context(JoinSnafu)?
            .context(DatabaseSnafu)?;

        let s = Self::from(create).init().await?;

        s.verify_self(self_id).await?;
        Ok(s)
    }

    async fn init(self) -> DbResult<Self> {
        self.write_with(|dbtx| {
            dbtx.open_table(&TABLE_DB_VER)?;
            dbtx.open_table(&TABLE_ID_SELF)?;
            dbtx.open_table(&TABLE_IDS)?;
            dbtx.open_table(&TABLE_ID_FOLLOWEES)?;
            dbtx.open_table(&TABLE_ID_FOLLOWERS)?;
            dbtx.open_table(&TABLE_ID_UNFOLLOWED)?;
            dbtx.open_table(&TABLE_ID_PERSONAS)?;
            dbtx.open_table(&TABLE_EVENTS)?;
            dbtx.open_table(&TABLE_EVENTS_BY_TIME)?;
            dbtx.open_table(&TABLE_EVENTS_SELF)?;
            dbtx.open_table(&TABLE_EVENTS_MISSING)?;
            dbtx.open_table(&TABLE_EVENTS_HEADS)?;

            Self::handle_db_ver_migrations(dbtx)?;

            Ok(())
        })
        .await?;

        Ok(self)
    }

    fn handle_db_ver_migrations(dbtx: &WriteTransaction) -> DbResult<()> {
        let mut table_db_ver = dbtx.open_table(&TABLE_DB_VER)?;

        let Some(cur_db_ver) = table_db_ver.first()?.map(|g| g.1.value()) else {
            info!(target: LOG_TARGET, "Initializing empty database");
            table_db_ver.insert(&(), &Self::DB_VER)?;

            return Ok(());
        };

        debug!(target: LOG_TARGET, db_ver = cur_db_ver, "Checking db version");
        if Self::DB_VER < cur_db_ver {
            return DbVersionTooHighSnafu {
                db_ver: cur_db_ver,
                code_ver: Self::DB_VER,
            }
            .fail();
        }

        // migration code will go here

        Ok(())
    }
}

impl From<redb_bincode::Database> for Database {
    fn from(db: redb_bincode::Database) -> Self {
        Self(db)
    }
}

impl Database {
    pub async fn write_with<T>(
        &self,
        f: impl FnOnce(&'_ WriteTransaction) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let mut dbtx = self.0.begin_write().context(TransactionSnafu)?;

            let res = f(&mut dbtx)?;

            dbtx.commit().context(CommitSnafu)?;

            Ok(res)
        })
    }

    pub async fn read_with<T>(
        &self,
        f: impl FnOnce(&'_ ReadTransaction) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let mut dbtx = self.0.begin_read().context(TransactionSnafu)?;

            f(&mut dbtx)
        })
    }

    pub async fn verify_self(&self, self_id: RostraId) -> DbResult<()> {
        self.write_with(|tx| {
            let mut id_self_table = tx.open_table(&TABLE_ID_SELF)?;

            if let Some(existing_id) = Self::read_self_id_tx(&id_self_table)? {
                if existing_id != self_id {
                    return IdMismatchSnafu.fail();
                }
            } else {
                Self::write_self_id_tx(self_id, &mut id_self_table)?;
            }
            Ok(())
        })
        .await
    }

    pub async fn get_head(&self, id: RostraId) -> DbResult<Option<ShortEventId>> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&TABLE_EVENTS_HEADS)?;

            Self::get_head_tx(id, &events_heads)
        })
        .await
    }

    pub async fn read_followees(
        &self,
        id: RostraId,
    ) -> DbResult<Vec<(RostraId, IdsFolloweesRecord)>> {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&TABLE_ID_FOLLOWEES)?;

            Self::read_followees_tx(id, &ids_followees_table)
        })
        .await
    }

    pub async fn read_followers(
        &self,
        id: RostraId,
    ) -> DbResult<Vec<(RostraId, IdsFollowersRecord)>> {
        self.read_with(|tx| {
            let ids_followers_table = tx.open_table(&TABLE_ID_FOLLOWERS)?;

            Self::read_followers_tx(id, &ids_followers_table)
        })
        .await
    }

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
        let event_id = ShortEventId::from(*event_id);
        if let Some(existing_content) = events_content_table.get(&event_id)?.map(|g| g.value()) {
            match existing_content {
                ContentState::Deleted { .. } => {
                    return Ok(false);
                }
                ContentState::Present(_) => {
                    return Ok(true);
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
        ContentFollow {
            followee,
            persona_id: persona,
        }: ContentFollow,

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

        Ok(true)
    }

    pub(crate) fn insert_unfollow_tx(
        author: RostraId,
        timestamp: Timestamp,
        ContentUnfollow { followee }: ContentUnfollow,

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

fn get_first_in_range<K, V>(
    events_table: &impl ReadableTable<K, V>,
    range: impl ops::RangeBounds<K>,
) -> Result<Option<K>, DbError>
where
    K: bincode::Decode + bincode::Encode,
    V: bincode::Decode + bincode::Encode,
{
    Ok(events_table
        .range(range)?
        .next()
        .transpose()?
        .map(|(k, _)| k.value()))
}

fn get_last_in_range<K, V>(
    events_table: &impl ReadableTable<K, V>,
    range: impl ops::RangeBounds<K>,
) -> Result<Option<K>, DbError>
where
    K: bincode::Decode + bincode::Encode,
    V: bincode::Decode + bincode::Encode,
{
    Ok(events_table
        .range(range)?
        .next_back()
        .transpose()?
        .map(|(k, _)| k.value()))
}

pub enum InsertEventOutcome {
    /// An event already existed, so it changed nothing
    AlreadyPresent,
    Inserted {
        /// An event already had a parent reporting its existence.
        ///
        /// This also implies that the event is not considered a "head event"
        /// anymore.
        was_missing: bool,
        /// This event was already marked as deleted by some processed children
        /// event
        is_deleted: bool,
        /// An existing parent event had its content marked as deleted.
        ///
        /// Note, if the parent event was marked for deletion, but it was not
        /// processed yet, this will not be set, and instead `is_deleted` will
        /// be set to true, when the deleted parent is processed.
        deleted_parent_content: Option<ShortEventId>,

        /// Ids of parents storage was not aware of yet, so they are now marked
        /// as "missing"
        missing_parents: Vec<ShortEventId>,
    },
}

#[cfg(test)]
mod tests;
