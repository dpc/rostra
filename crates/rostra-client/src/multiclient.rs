use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraId;
use rostra_util_error::FmtCompact as _;
use snafu::{ResultExt as _, Snafu};
use tokio::sync::RwLock;
use tracing::warn;

use crate::error::InitError;
use crate::{Client, ClientHandle, LOG_TARGET};

#[derive(Debug, Snafu)]
pub enum MultiClientError {
    ClientInit {
        source: InitError,
    },
    Database {
        source: DbError,
    },
    #[snafu(transparent)]
    Io {
        source: io::Error,
    },
}

pub type MultiClientResult<T> = std::result::Result<T, MultiClientError>;
pub struct MultiClient {
    data_dir: PathBuf,
    inner: tokio::sync::RwLock<HashMap<RostraId, Arc<Client>>>,
}

impl MultiClient {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            inner: RwLock::new(Default::default()),
        }
    }
}

impl MultiClient {
    pub async fn load(&self, id: RostraId) -> MultiClientResult<Arc<Client>> {
        let mut write = self.inner.write().await;
        if let Some(c) = write.get(&id) {
            return Ok(c.clone());
        }

        let db_path = Database::mk_db_path(&self.data_dir, id).await?;

        let compact = db_path.exists();
        let mut db = Database::open(&db_path, id).await.context(DatabaseSnafu)?;

        if compact {
            if let Err(err) = db.compact().await {
                warn!(
                    target: LOG_TARGET,
                    err = %err.fmt_compact(),
                    path=%db_path.display(),
                    "Failed to compact database"
                );
            }
        }
        let client = Client::builder(id)
            .db(db)
            .build()
            .await
            .context(ClientInitSnafu)?;

        write.insert(id, client.clone());
        Ok(client)
    }

    pub async fn get(&self, id: RostraId) -> Option<ClientHandle> {
        self.inner.read().await.get(&id).map(|c| c.handle())
    }
}
