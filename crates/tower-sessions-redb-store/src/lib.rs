//! Redb-based session store for tower-sessions.
//!
//! This crate provides a persistent session store using redb, allowing
//! sessions to survive server restarts.
//!
//! # Example
//!
//! ```ignore
//! use tower_sessions_redb_store::RedbSessionStore;
//! use tower_sessions::SessionManagerLayer;
//! use std::sync::Arc;
//!
//! let db = redb::Database::create("app.redb")?;
//! let db = Arc::new(redb_bincode::Database::from(db));
//! let session_store = RedbSessionStore::new(db)?;
//! let session_layer = SessionManagerLayer::new(session_store);
//! ```

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use bincode::{Decode, Encode};
use redb_bincode::TableDefinition;
use time::OffsetDateTime;
use tower_sessions_core::session::{Id, Record};
use tower_sessions_core::session_store::{self, SessionStore};

/// Session record for storage in redb.
///
/// We store this instead of `Record` directly because we need to serialize
/// the expiry as a Unix timestamp (OffsetDateTime doesn't implement bincode's
/// Encode/Decode directly).
#[derive(Debug, Clone, Encode, Decode)]
struct StoredSession {
    /// Session data as JSON bytes (HashMap<String, Value>)
    data: Vec<u8>,
    /// Expiry as Unix timestamp (seconds since epoch)
    expiry_unix: i64,
}

impl StoredSession {
    fn from_record(record: &Record) -> Result<Self, session_store::Error> {
        let data = serde_json::to_vec(&record.data)
            .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        Ok(Self {
            data,
            expiry_unix: record.expiry_date.unix_timestamp(),
        })
    }

    fn into_record(self, id: Id) -> Result<Record, session_store::Error> {
        let data = serde_json::from_slice(&self.data)
            .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        let expiry_date = OffsetDateTime::from_unix_timestamp(self.expiry_unix)
            .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        Ok(Record {
            id,
            data,
            expiry_date,
        })
    }

    fn is_expired(&self) -> bool {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        self.expiry_unix < now
    }
}

/// Table definition for sessions.
///
/// Key: session ID as i128
/// Value: serialized session data
const SESSIONS_TABLE: TableDefinition<i128, StoredSession> =
    TableDefinition::new("tower_sessions_redb_store::sessions");

/// Error type for session store initialization.
#[derive(Debug)]
pub enum SessionStoreError {
    Database(redb::DatabaseError),
    Transaction(Box<redb::TransactionError>),
    Table(redb::TableError),
    Commit(redb::CommitError),
}

impl fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(e) => write!(f, "database error: {e}"),
            Self::Transaction(e) => write!(f, "transaction error: {e}"),
            Self::Table(e) => write!(f, "table error: {e}"),
            Self::Commit(e) => write!(f, "commit error: {e}"),
        }
    }
}

impl std::error::Error for SessionStoreError {}

impl From<redb::DatabaseError> for SessionStoreError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Database(e)
    }
}

impl From<redb::TransactionError> for SessionStoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Transaction(Box::new(e))
    }
}

impl From<redb::TableError> for SessionStoreError {
    fn from(e: redb::TableError) -> Self {
        Self::Table(e)
    }
}

impl From<redb::CommitError> for SessionStoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::Commit(e)
    }
}

/// A redb-based session store for tower-sessions.
#[derive(Debug, Clone)]
pub struct RedbSessionStore {
    db: Arc<redb_bincode::Database>,
}

impl RedbSessionStore {
    /// Create a new session store using the provided database.
    ///
    /// The database should be shared across all components that need it.
    /// This function initializes the sessions table if it doesn't exist.
    ///
    /// This performs blocking I/O, so call from a blocking context or use
    /// `spawn_blocking`.
    pub fn new(db: Arc<redb_bincode::Database>) -> Result<Self, SessionStoreError> {
        // Initialize the table
        {
            let write_txn = db.begin_write()?;
            // Opening the table in a write transaction ensures it exists
            let _ = write_txn.open_table(&SESSIONS_TABLE)?;
            write_txn.commit()?;
        }

        Ok(Self { db })
    }
}

#[async_trait]
impl SessionStore for RedbSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        self.save(record).await
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let stored = StoredSession::from_record(record)?;
        let id = record.id.0;

        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let write_txn = db
                .begin_write()
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            {
                let mut table = write_txn
                    .open_table(&SESSIONS_TABLE)
                    .map_err(|e| session_store::Error::Backend(e.to_string()))?;
                table
                    .insert(&id, &stored)
                    .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            }
            write_txn
                .commit()
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| session_store::Error::Backend(e.to_string()))?
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let id = session_id.0;
        let session_id = *session_id;

        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let read_txn = db
                .begin_read()
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            let table = read_txn
                .open_table(&SESSIONS_TABLE)
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;

            let Some(stored) = table
                .get(&id)
                .map_err(|e| session_store::Error::Backend(e.to_string()))?
                .map(|g| g.value())
            else {
                return Ok(None);
            };

            // Check if expired
            if stored.is_expired() {
                return Ok(None);
            }

            stored.into_record(session_id).map(Some)
        })
        .await
        .map_err(|e| session_store::Error::Backend(e.to_string()))?
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let id = session_id.0;

        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let write_txn = db
                .begin_write()
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            {
                let mut table = write_txn
                    .open_table(&SESSIONS_TABLE)
                    .map_err(|e| session_store::Error::Backend(e.to_string()))?;
                table
                    .remove(&id)
                    .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            }
            write_txn
                .commit()
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| session_store::Error::Backend(e.to_string()))?
    }
}
