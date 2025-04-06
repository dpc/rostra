use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraId;
use rostra_util_error::FmtCompact as _;
use snafu::{ResultExt as _, Snafu};
use tokio::sync::RwLock;
use tracing::{info, warn};

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
struct ClientInfo {
    client: Arc<Client>,
    last_used: Instant,
}

pub struct MultiClient {
    data_dir: PathBuf,
    inner: tokio::sync::RwLock<HashMap<RostraId, ClientInfo>>,
    max_clients: usize,
    usage_queue: tokio::sync::RwLock<VecDeque<RostraId>>,
}

impl MultiClient {
    pub fn new(data_dir: PathBuf, max_clients: usize) -> Self {
        Self {
            data_dir,
            inner: RwLock::new(Default::default()),
            max_clients: max_clients.max(1), // Ensure at least 1 client
            usage_queue: RwLock::new(VecDeque::new()),
        }
    }
}

impl MultiClient {
    pub async fn load(&self, id: RostraId) -> MultiClientResult<Arc<Client>> {
        // First check if the client is already loaded
        {
            let mut write = self.inner.write().await;
            if let Some(client_info) = write.get_mut(&id) {
                // Update last used time
                client_info.last_used = Instant::now();

                // Update usage queue
                self.update_usage_queue(id).await;

                return Ok(client_info.client.clone());
            }
        }

        // Client not loaded, need to load it
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

        // Check if we need to evict clients
        self.maybe_evict_clients().await?;

        // Insert the new client
        {
            let mut write = self.inner.write().await;
            write.insert(
                id,
                ClientInfo {
                    client: client.clone(),
                    last_used: Instant::now(),
                },
            );
        }

        // Update usage queue
        self.update_usage_queue(id).await;

        Ok(client)
    }

    // Helper method to update the usage queue
    async fn update_usage_queue(&self, id: RostraId) {
        let mut queue = self.usage_queue.write().await;

        // Remove the ID if it's already in the queue
        if let Some(pos) = queue.iter().position(|&x| x == id) {
            queue.remove(pos);
        }

        // Add the ID to the front of the queue
        queue.push_front(id);
    }

    // Helper method to evict clients if we're over the limit
    async fn maybe_evict_clients(&self) -> MultiClientResult<()> {
        let mut write = self.inner.write().await;

        // If we're under the limit, no need to evict
        if write.len() < self.max_clients {
            return Ok(());
        }

        // Get the least recently used clients from the queue
        let to_evict = {
            let queue = self.usage_queue.read().await;

            // Get the IDs of clients to evict (from the back of the queue)
            let num_to_evict = write.len() + 1 - self.max_clients;
            queue
                .iter()
                .rev()
                .take(num_to_evict)
                .cloned()
                .collect::<Vec<_>>()
        };

        // Evict the clients
        for id in to_evict {
            if write.remove(&id).is_some() {
                info!(
                    target: LOG_TARGET,
                    id = %id,
                    "Evicted client due to max_clients limit"
                );
            }
        }

        // Update the usage queue
        {
            let mut queue = self.usage_queue.write().await;
            queue.retain(|id| write.contains_key(id));
        }

        Ok(())
    }

    pub async fn get(&self, id: RostraId) -> Option<ClientHandle> {
        let mut write = self.inner.write().await;
        if let Some(client_info) = write.get_mut(&id) {
            // Update last used time
            client_info.last_used = Instant::now();

            // Update usage queue
            self.update_usage_queue(id).await;

            Some(client_info.client.handle())
        } else {
            None
        }
    }
}
