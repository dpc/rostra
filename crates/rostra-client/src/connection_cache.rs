use std::collections::HashMap;
use std::sync::Arc;

use rostra_core::id::RostraId;
use rostra_p2p::Connection;
use tokio::sync::{Mutex, OnceCell};
use tracing::trace;

use crate::ClientRef;

const LOG_TARGET: &str = "rostra-client::connection-cache";

type LazySharedConnection = Arc<OnceCell<Option<Connection>>>;

#[derive(Clone)]
pub struct ConnectionCache {
    connections: Arc<Mutex<HashMap<RostraId, LazySharedConnection>>>,
}

impl Default for ConnectionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionCache {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get_or_connect(&self, client: &ClientRef<'_>, id: RostraId) -> Option<Connection> {
        let mut pool_lock = self.connections.lock().await;

        let entry_arc = pool_lock
            .entry(id)
            .and_modify(|entry_arc| {
                // Check if existing connection is disconnected and remove it
                if let Some(Some(existing_conn)) = entry_arc.get()
                    && existing_conn.is_closed() {
                        trace!(target: LOG_TARGET, %id, "Existing connection is disconnected, removing from pool");
                        *entry_arc = Arc::new(OnceCell::new());
                    }
            })
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        // Drop the pool lock so other connections can work in parallel
        drop(pool_lock);

        let result = entry_arc
            .get_or_init(|| async {
                trace!(target: LOG_TARGET, %id, "Creating new connection");
                match client.connect_uncached(id).await {
                    Ok(conn) => {
                        trace!(target: LOG_TARGET, %id, "Connection successful");
                        Some(conn)
                    }
                    Err(err) => {
                        trace!(target: LOG_TARGET, %id, err = %err, "Connection failed");
                        None
                    }
                }
            })
            .await;

        result.clone()
    }
}
