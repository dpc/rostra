use std::collections::HashMap;
use std::sync::Arc;

use futures::stream::{self, StreamExt as _};
use rostra_core::ShortEventId;
use rostra_core::event::{VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, info, trace};

use crate::error::ConnectResult;
use crate::net::ClientNetworking;

const LOG_TARGET: &str = "rostra-client::connection-cache";

type LazySharedConnection = Arc<OnceCell<Connection>>;

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

    pub async fn get_or_connect(
        &self,
        networking: &ClientNetworking,
        id: RostraId,
    ) -> ConnectResult<Connection> {
        let mut pool_lock = self.connections.lock().await;

        let entry_arc = pool_lock
            .entry(id)
            .and_modify(|entry_arc| {
                // Check if existing connection is disconnected and remove it
                if let Some(existing_conn) = entry_arc.get()
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
            .get_or_try_init(|| async {
                trace!(target: LOG_TARGET, %id, "Creating new connection");
                match networking.connect_uncached(id).await {
                    Ok(conn) => {
                        info!(target: LOG_TARGET, %id, endpoint_id = %conn.remote_id().fmt_short(), "Connection successful");
                        Ok(conn)
                    }
                    Err(err) => {
                        debug!(target: LOG_TARGET, %id, err = %err, "Connection failed");
                        Err(err)
                    }
                }
            })
            .await;

        result.cloned()
    }

    /// Try to fetch an event from multiple peers with some parallelism.
    ///
    /// Returns `Some(event)` from the first peer that has it, or `None`.
    pub async fn get_event_from_peers(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        author_id: RostraId,
        event_id: ShortEventId,
    ) -> Option<VerifiedEvent> {
        let result = futures_lite::StreamExt::find_map(
            &mut stream::iter(peers.iter().copied())
                .map(|peer_id| {
                    let cache = self.clone();
                    async move {
                        let conn = cache.get_or_connect(networking, peer_id).await.ok()?;
                        match conn.get_event(author_id, event_id).await {
                            Ok(Some(event)) => Some(event),
                            Ok(None) => {
                                debug!(
                                    target: LOG_TARGET,
                                    peer_id = %peer_id.to_short(),
                                    event_id = %event_id.to_short(),
                                    "Event not found on peer"
                                );
                                None
                            }
                            Err(_err) => {
                                debug!(
                                    target: LOG_TARGET,
                                    peer_id = %peer_id.to_short(),
                                    event_id = %event_id.to_short(),
                                    "Failed to fetch event from peer"
                                );
                                None
                            }
                        }
                    }
                })
                .buffer_unordered(4),
            |result| result,
        )
        .await;

        if result.is_none() {
            debug!(
                target: LOG_TARGET,
                event_id = %event_id.to_short(),
                "Event not found on any peer"
            );
        }

        result
    }

    /// Try to fetch event content from multiple peers with some parallelism.
    ///
    /// Returns `Some(content)` from the first peer that has it, or `None`.
    pub async fn get_event_content_from_peers(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        event: VerifiedEvent,
    ) -> Option<VerifiedEventContent> {
        let result = futures_lite::StreamExt::find_map(
            &mut stream::iter(peers.iter().copied())
                .map(|peer_id| {
                    let cache = self.clone();
                    async move {
                        let conn = cache.get_or_connect(networking, peer_id).await.ok()?;
                        match conn.get_event_content(event).await {
                            Ok(Some(content)) => Some(content),
                            Ok(None) => {
                                debug!(
                                    target: LOG_TARGET,
                                    peer_id = %peer_id.to_short(),
                                    event_id = %event.event_id.to_short(),
                                    "Peer does not have content"
                                );
                                None
                            }
                            Err(_err) => {
                                debug!(
                                    target: LOG_TARGET,
                                    peer_id = %peer_id.to_short(),
                                    event_id = %event.event_id.to_short(),
                                    "Failed to fetch content from peer"
                                );
                                None
                            }
                        }
                    }
                })
                .buffer_unordered(4),
            |result| result,
        )
        .await;

        if result.is_none() {
            debug!(
                target: LOG_TARGET,
                event_id = %event.event_id.to_short(),
                "Event content not found from any peer"
            );
        }

        result
    }
}
