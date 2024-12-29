mod models;
mod tables;

use std::path::PathBuf;

use redb_bincode::{ReadTransaction, WriteTransaction};
use rostra_util_error::BoxedError;
use snafu::{Location, ResultExt as _, Snafu};
use tables::{
    TABLE_DB_VER, TABLE_EVENTS, TABLE_EVENTS_HEADS, TABLE_EVENTS_MISSING, TABLE_IDS,
    TABLE_ID_SOCIAL_FOLLOWING, TABLE_SELF,
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
            dbtx.open_table(&TABLE_ID_SOCIAL_FOLLOWING)
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

        let Some(cur_db_ver) = table_db_ver
            .first()
            .context(StorageSnafu)?
            .map(|g| g.1.value())
        else {
            info!(target: LOG_TARGET, "Initializing empty database");
            table_db_ver
                .insert(&(), &Self::DB_VER)
                .context(StorageSnafu)?;

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
}
