use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraId;
use snafu::{ResultExt as _, Snafu};
use tokio::sync::RwLock;

use crate::error::InitError;
use crate::{Client, ClientHandle};

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

        let client = Client::builder(id)
            .db(
                Database::open(Database::mk_db_path(&self.data_dir, id).await?, id)
                    .await
                    .context(DatabaseSnafu)?,
            )
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
