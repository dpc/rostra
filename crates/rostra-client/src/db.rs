mod models;
mod tables;

use std::path::PathBuf;

use redb_bincode::{ReadTransaction, ReadableTable, Table, WriteTransaction};
use rostra_core::event::{SignedEvent, VerifiedEvent};
use rostra_core::id::{RostraId, ShortRostraId};
use rostra_core::ShortEventId;
use rostra_util_error::BoxedError;
use snafu::{Location, ResultExt as _, Snafu};
use tables::events::EventsMissingRecord;
use tables::ids::{IdFollowingRecord, IdRecord};
use tables::{
    ContentState, EventRecord, EventsHeadsTableValue, TABLE_DB_VER, TABLE_EVENTS,
    TABLE_EVENTS_HEADS, TABLE_EVENTS_MISSING, TABLE_IDS, TABLE_IDS_FOLLOWING, TABLE_SELF,
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

    pub async fn init(self) -> DbResult<Self> {
        self.write_with(|dbtx| {
            dbtx.open_table(&TABLE_DB_VER).context(TableSnafu)?;
            dbtx.open_table(&TABLE_EVENTS).context(TableSnafu)?;
            dbtx.open_table(&TABLE_SELF).context(TableSnafu)?;
            dbtx.open_table(&TABLE_IDS).context(TableSnafu)?;
            dbtx.open_table(&TABLE_IDS_FOLLOWING).context(TableSnafu)?;
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

    pub async fn read_following(&self) -> DbResult<Vec<(RostraId, IdRecord)>> {
        self.read_with(|tx| {
            let ids_table = tx.open_table(&TABLE_IDS).context(TableSnafu)?;
            let ids_following_table = tx.open_table(&TABLE_IDS_FOLLOWING).context(TableSnafu)?;

            Self::read_following_tx(&ids_table, &ids_following_table)
        })
        .await
    }

    pub fn read_following_tx(
        ids_table: &impl ReadableTable<ShortRostraId, IdRecord>,
        ids_following_table: &impl ReadableTable<ShortRostraId, IdFollowingRecord>,
    ) -> DbResult<Vec<(RostraId, IdRecord)>> {
        let short_ids = ids_following_table.range(..)?;

        let mut ids = vec![];

        for short_id in short_ids {
            let short_id = short_id?.0.value();

            let id_record = ids_table
                .get(&short_id)?
                .expect("Must have entry in ids table for every one in ids-following table")
                .value();
            ids.push((RostraId::assemble(short_id, id_record.id_rest), id_record));
        }
        Ok(ids)
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
                if event.is_delete_aux_set() && is_aux {
                    // keep the existing deleted mark if there, otherwise mark as deleted by the
                    // current event
                    prev_event.deleted = prev_event.deleted.or(Some(event_id));
                    prev_event.content = ContentState::Deleted;
                    events_table.insert(&prev_id, &prev_event)?;
                }
            } else {
                // we do not have this parent yet, so we mark it as missing
                events_missing_table.insert(
                    &(short_author, prev_id),
                    &EventsMissingRecord {
                        // potentially mark that the missing event was already deleted
                        deleted: (event.is_delete_aux_set() && is_aux).then_some(event_id),
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
                deleted,
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
}

#[cfg(test)]
mod tests;
