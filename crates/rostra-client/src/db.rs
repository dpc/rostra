mod models;
mod tables;

use std::path::PathBuf;

use redb_bincode::{ReadTransaction, ReadableTable, Table, WriteTransaction};
use rostra_core::event::{SignedEvent, VerifiedEvent};
use rostra_core::id::{RostraId, ShortRostraId};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::BoxedError;
use snafu::{Location, ResultExt as _, Snafu};
use tables::events::EventsMissingRecord;
use tables::ids::{IdsFolloweesRecord, IdsFolloweesTsRecord};
use tables::{
    ContentState, EventRecord, EventsHeadsTableValue, TABLE_DB_VER, TABLE_EVENTS,
    TABLE_EVENTS_HEADS, TABLE_EVENTS_MISSING, TABLE_IDS, TABLE_IDS_FOLLOWEES,
    TABLE_IDS_FOLLOWEES_TS, TABLE_SELF,
};
use tokio::task::JoinError;
use tracing::{debug, info, instrument};

const LOG_TARGET: &str = "rostra::client::db";

#[derive(Debug, Snafu)]
pub enum DbError {
    Database {
        source: redb::DatabaseError,
        #[snafu(implicit)]
        location: Location,
    },
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
}
pub type DbResult<T> = std::result::Result<T, DbError>;

#[derive(Debug)]
pub struct Database(redb_bincode::Database);

impl Database {
    const DB_VER: u64 = 0;

    async fn init(self) -> DbResult<Self> {
        self.write_with(|dbtx| {
            dbtx.open_table(&TABLE_DB_VER).context(TableSnafu)?;
            dbtx.open_table(&TABLE_EVENTS).context(TableSnafu)?;
            dbtx.open_table(&TABLE_SELF).context(TableSnafu)?;
            dbtx.open_table(&TABLE_IDS).context(TableSnafu)?;
            dbtx.open_table(&TABLE_IDS_FOLLOWEES).context(TableSnafu)?;
            dbtx.open_table(&TABLE_IDS_FOLLOWEES_TS)
                .context(TableSnafu)?;
            dbtx.open_table(&TABLE_EVENTS).context(TableSnafu)?;
            dbtx.open_table(&TABLE_EVENTS_MISSING).context(TableSnafu)?;
            dbtx.open_table(&TABLE_EVENTS_HEADS).context(TableSnafu)?;

            Self::handle_db_ver_migrations(dbtx)?;

            Ok(())
        })
        .await?;

        Ok(self)
    }

    fn handle_db_ver_migrations(dbtx: &WriteTransaction) -> DbResult<()> {
        let mut table_db_ver = dbtx.open_table(&TABLE_DB_VER).context(TableSnafu)?;

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

    #[instrument(skip_all)]
    pub async fn open(path: impl Into<PathBuf>) -> DbResult<Database> {
        let path = path.into();
        let create = tokio::task::spawn_blocking(move || redb_bincode::Database::create(path))
            .await
            .context(JoinSnafu)?
            .context(DatabaseSnafu)?;
        Self::from(create).init().await
    }

    pub async fn read_followees(&self, id: ShortRostraId) -> DbResult<Vec<(RostraId, String)>> {
        self.read_with(|tx| {
            let ids_following_table = tx.open_table(&TABLE_IDS_FOLLOWEES).context(TableSnafu)?;

            Self::read_followees_tx(id, &ids_following_table)
        })
        .await
    }

    pub fn read_followees_tx(
        id: ShortRostraId,
        ids_following_table: &impl ReadableTable<(ShortRostraId, RostraId), IdsFolloweesRecord>,
    ) -> DbResult<Vec<(RostraId, String)>> {
        Ok(ids_following_table
            .range((id, RostraId::ZERO)..=(id, RostraId::MAX))?
            .map(|res| res.map(|(k, v)| (k.value().1, v.value().persona)))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn insert_event_tx(
        VerifiedEvent {
            event_id,
            event,
            sig,
            content,
        }: &VerifiedEvent,
        events_table: &mut Table<ShortEventId, EventRecord>,
        events_missing_table: &mut Table<(ShortRostraId, ShortEventId), EventsMissingRecord>,
        events_heads_table: &mut Table<(ShortRostraId, ShortEventId), EventsHeadsTableValue>,
    ) -> DbResult<()> {
        let author = event.author;
        let event_id = ShortEventId::from(*event_id);
        let short_author = ShortRostraId::from(author);

        let existing = events_table.get(&event_id)?.map(|g| g.value());
        if let Some(mut existing) = existing {
            match existing.content {
                ContentState::Deleted | ContentState::Present(_) => {}
                ContentState::Missing => {
                    if let Some(content) = content {
                        existing.content = ContentState::Present(content.to_owned());
                        events_table.insert(&event_id, &existing)?;
                    }
                }
            }
            return Ok(());
        }

        let deleted = if let Some(prev_missing) = events_missing_table
            .remove(&(short_author, event_id))?
            .map(|g| g.value())
        {
            // if the missing was marked as deleted, we'll record it in the newly added
            // event
            prev_missing.deleted
        } else {
            // since nothing was expecting this event yet, it must be a "head"
            events_heads_table.insert(&(short_author, event_id), &EventsHeadsTableValue)?;

            None
        };

        // When both parents point at same thing, process only one: one that can
        // be responsible for deletion.
        let prev_ids = if event.parent_aux == event.parent_prev {
            vec![(event.parent_aux, true)]
        } else {
            vec![(event.parent_aux, true), (event.parent_prev, false)]
        };

        for (prev_id, is_aux) in prev_ids {
            if prev_id == ShortEventId::ZERO {
                continue;
            }

            let prev_event = events_table.get(&prev_id)?.map(|r| r.value());
            if let Some(mut prev_event) = prev_event {
                if event.is_delete_parent_aux_set() && is_aux {
                    // keep the existing deleted mark if there, otherwise mark as deleted by the
                    // current event
                    prev_event.deleted_by = prev_event.deleted_by.or(Some(event_id));
                    prev_event.content = ContentState::Deleted;
                    events_table.insert(&prev_id, &prev_event)?;
                }
            } else {
                // we do not have this parent yet, so we mark it as missing
                events_missing_table.insert(
                    &(short_author, prev_id),
                    &EventsMissingRecord {
                        // potentially mark that the missing event was already deleted
                        deleted: (event.is_delete_parent_aux_set() && is_aux).then_some(event_id),
                    },
                )?;
            }
            // if the event was considered a "head", it shouldn't as it has a child
            events_heads_table.remove(&(short_author, prev_id))?;
        }

        events_table.insert(
            &event_id,
            &EventRecord {
                event: SignedEvent {
                    event: *event,
                    sig: *sig,
                },
                deleted_by: deleted,
                content: if deleted.is_some() {
                    ContentState::Deleted
                } else {
                    ContentState::from(content.to_owned())
                },
            },
        )?;

        Ok(())
    }

    pub fn get_missing_events_tx(
        author: impl Into<ShortRostraId>,
        events_missing_table: &impl ReadableTable<(ShortRostraId, ShortEventId), EventsMissingRecord>,
    ) -> DbResult<Vec<ShortEventId>> {
        let author = author.into();
        Ok(events_missing_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _v)| k.value().1))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_heads_events_tx(
        author: impl Into<ShortRostraId>,
        events_heads_table: &impl ReadableTable<(ShortRostraId, ShortEventId), EventsHeadsTableValue>,
    ) -> DbResult<Vec<ShortEventId>> {
        let author = author.into();
        Ok(events_heads_table
            .range((author, ShortEventId::ZERO)..=(author, ShortEventId::MAX))?
            .map(|r| r.map(|(k, _v)| k.value().1))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_event(
        event: impl Into<ShortEventId>,
        events_table: &impl ReadableTable<ShortEventId, EventRecord>,
    ) -> DbResult<Option<EventRecord>> {
        Ok(events_table.get(&event.into())?.map(|r| r.value()))
    }

    pub fn insert_followee_update(
        author: ShortRostraId,
        ts: Timestamp,
        followees: Vec<(RostraId, String)>,
        ids_folowees_ts_table: &mut Table<ShortRostraId, IdsFolloweesTsRecord>,
        ids_folowees_table: &mut Table<(ShortRostraId, RostraId), IdsFolloweesRecord>,
    ) -> DbResult<bool> {
        if ids_folowees_ts_table
            .get(&author)?
            .is_some_and(|existing| u64::from(ts) <= existing.value().ts)
        {
            return Ok(false);
        }

        ids_folowees_table.retain_in(
            (author, RostraId::ZERO)..=(author, RostraId::MAX),
            |_k, _v| false,
        )?;

        for (followee_id, persona) in followees {
            ids_folowees_table.insert(&(author, followee_id), &IdsFolloweesRecord { persona })?;
        }

        return Ok(true);
    }
}

#[cfg(test)]
mod tests;
