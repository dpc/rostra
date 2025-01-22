mod models;
mod tables;
mod tx_ops;

use std::path::PathBuf;
use std::{ops, result};

use ids::IdsFollowersRecord;
use redb_bincode::{ReadTransaction, ReadableTable, WriteTransaction};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_util_error::BoxedError;
use snafu::{Location, ResultExt as _, Snafu};
use tables::ids::IdsFolloweesRecord;
use tokio::task::JoinError;
use tracing::{debug, info, instrument};

pub use self::tables::*;

const LOG_TARGET: &str = "rostra::db";

pub struct WriteTransactionCtx {
    dbtx: WriteTransaction,
    on_commit: std::sync::Mutex<Vec<Box<dyn FnOnce() + 'static>>>,
}

impl From<WriteTransaction> for WriteTransactionCtx {
    fn from(dbtx: WriteTransaction) -> Self {
        Self {
            dbtx,
            on_commit: std::sync::Mutex::new(vec![]),
        }
    }
}
impl ops::Deref for WriteTransactionCtx {
    type Target = WriteTransaction;

    fn deref(&self) -> &Self::Target {
        &self.dbtx
    }
}

impl ops::DerefMut for WriteTransactionCtx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.dbtx
    }
}

impl WriteTransactionCtx {
    pub fn on_commit(&self, f: impl FnOnce() + 'static) {
        self.on_commit
            .lock()
            .expect("Locking failed")
            .push(Box::new(f));
    }

    fn commit(self) -> result::Result<(), redb::CommitError> {
        let Self { dbtx, on_commit } = self;

        dbtx.commit()?;

        for hook in on_commit.lock().expect("Locking failed").drain(..) {
            hook();
        }
        Ok(())
    }
}

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
            // dbtx.open_table(&db_ver::TABLE)?;
            // dbtx.open_table(&id_self::TABLE)?;
            // dbtx.open_table(&id::TABLE)?;
            // dbtx.open_table(&events_id_followees::TABLE)?;
            // dbtx.open_table(&TABLE_ID_UNFOLLOWED)?;
            // dbtx.open_table(&events_id_personas::TABLE)?;
            // dbtx.open_table(&events::TABLE)?;
            // dbtx.open_table(&events_by_time::TABLE)?;
            // dbtx.open_table(&events_content::TABLE)?;
            // dbtx.open_table(&events_self::TABLE)?;
            // dbtx.open_table(&events_missing::TABLE)?;
            // dbtx.open_table(&events_heads::TABLE)?;

            Self::handle_db_ver_migrations(dbtx)?;

            Ok(())
        })
        .await?;

        Ok(self)
    }

    fn handle_db_ver_migrations(dbtx: &WriteTransaction) -> DbResult<()> {
        let mut table_db_ver = dbtx.open_table(&db_migration_ver::TABLE)?;

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
        f: impl FnOnce(&'_ WriteTransactionCtx) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let mut dbtx =
                WriteTransactionCtx::from(self.0.begin_write().context(TransactionSnafu)?);
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

    pub async fn iroh_secret(&self) -> DbResult<iroh::SecretKey> {
        self.read_with(|tx| {
            let id_self_table = tx.open_table(&ids_self::TABLE)?;

            let self_id = Self::read_self_id_tx(&id_self_table)?
                .expect("Must have iroh secret generated after opening");
            Ok(iroh::SecretKey::from_bytes(&self_id.iroh_secret))
        })
        .await
    }

    pub async fn verify_self(&self, self_id: RostraId) -> DbResult<()> {
        self.write_with(|tx| {
            let mut id_self_table = tx.open_table(&ids_self::TABLE)?;

            if let Some(existing_self_id_record) = Self::read_self_id_tx(&id_self_table)? {
                if existing_self_id_record.rostra_id != self_id {
                    return IdMismatchSnafu.fail();
                }
            } else {
                Self::write_self_id_tx(self_id, &mut id_self_table)?;
            };
            Ok(())
        })
        .await
    }

    pub async fn get_head(&self, id: RostraId) -> DbResult<Option<ShortEventId>> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_head_tx(id, &events_heads)
        })
        .await
    }

    pub async fn read_followees(
        &self,
        id: RostraId,
    ) -> DbResult<Vec<(RostraId, IdsFolloweesRecord)>> {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;

            Self::read_followees_tx(id, &ids_followees_table)
        })
        .await
    }

    pub async fn read_followers(
        &self,
        id: RostraId,
    ) -> DbResult<Vec<(RostraId, IdsFollowersRecord)>> {
        self.read_with(|tx| {
            let ids_followers_table = tx.open_table(&ids_followers::TABLE)?;

            Self::read_followers_tx(id, &ids_followers_table)
        })
        .await
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
