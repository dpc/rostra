use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use rostra_client_db::{Database, IdsFollowersRecord, WotData};
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::FmtCompact as _;
use tokio::sync::watch;
use tracing::{debug, info, instrument, trace, warn};

use crate::client::Client;
use crate::connection_cache::ConnectionCache;

const LOG_TARGET: &str = "rostra::poll_follower_heads";

/// Polls followers for new head updates using the WAIT_FOLLOWERS_NEW_HEADS RPC.
///
/// This task maintains connections to self and direct followers, polling each
/// for head updates using a blocking RPC call. When a new head is discovered,
/// the event is verified and added to the database.
pub struct PollFollowerHeadUpdates {
    client: crate::client::ClientHandle,
    db: Arc<Database>,
    self_id: RostraId,
    self_followers_rx: watch::Receiver<Arc<HashMap<RostraId, IdsFollowersRecord>>>,
    self_wot_rx: watch::Receiver<Arc<WotData>>,
    connections: ConnectionCache,
}

impl PollFollowerHeadUpdates {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting poll follower head updates task");
        Self {
            client: client.handle(),
            db: client.db().clone(),
            self_id: client.rostra_id(),
            self_followers_rx: client.self_followers_subscribe(),
            self_wot_rx: client.self_wot_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "poll-follower-head-updates", skip(self), ret)]
    pub async fn run(mut self) {
        // Keep track of which peers we're polling
        let mut active_peers: HashSet<RostraId> = HashSet::new();
        // FuturesUnordered to manage concurrent polling tasks
        let mut poll_futures: FuturesUnordered<_> = FuturesUnordered::new();

        // Start with self
        active_peers.insert(self.self_id);

        loop {
            // Check for follower changes to update active peers
            let current_followers: HashSet<RostraId> = {
                let followers = self.self_followers_rx.borrow();
                followers.keys().copied().collect()
            };

            // Add new followers
            let new_followers: Vec<_> = current_followers
                .difference(&active_peers)
                .copied()
                .collect();
            for follower_id in new_followers {
                debug!(target: LOG_TARGET, %follower_id, "Adding follower to poll list");
                active_peers.insert(follower_id);
            }

            // Note: We don't remove peers that are no longer followers since existing
            // connections will naturally close when the RPC fails.

            // Spawn polling tasks for peers that don't have active connections
            let peers_to_poll: Vec<_> = active_peers.iter().copied().collect();
            for peer_id in peers_to_poll {
                let client = self.client.clone();
                let connections = self.connections.clone();
                let db = self.db.clone();
                let self_id = self.self_id;
                let wot_rx = self.self_wot_rx.clone();

                poll_futures.push(async move {
                    Self::poll_peer_for_heads(client, connections, db, self_id, peer_id, wot_rx)
                        .await;
                    peer_id
                });
            }

            // Clear active peers - they'll be re-added based on poll results
            active_peers.clear();
            active_peers.insert(self.self_id);

            tokio::select! {
                // Wait for any polling task to complete
                Some(peer_id) = poll_futures.next() => {
                    trace!(target: LOG_TARGET, %peer_id, "Poll task completed, will restart");
                    // Re-add the peer to be polled again
                    active_peers.insert(peer_id);
                }
                // Check for follower changes
                res = self.self_followers_rx.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "Followers channel closed, shutting down");
                        break;
                    }
                    debug!(target: LOG_TARGET, "Followers changed, updating poll list");
                }
            }

            // Check if client is still running
            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            }
        }
    }

    async fn poll_peer_for_heads(
        client: crate::client::ClientHandle,
        connections: ConnectionCache,
        db: Arc<Database>,
        self_id: RostraId,
        peer_id: RostraId,
        wot_rx: watch::Receiver<Arc<WotData>>,
    ) {
        loop {
            let Ok(client_ref) = client.client_ref() else {
                break;
            };

            let conn = match connections.get_or_connect(&client_ref, peer_id).await {
                Ok(conn) => conn,
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        peer_id = %peer_id.to_short(),
                        err = %err.fmt_compact(),
                        "Could not connect to peer for polling"
                    );
                    // Wait a bit before retrying
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    continue;
                }
            };

            match Self::poll_once(&conn, &db, self_id, &wot_rx).await {
                Ok(()) => {
                    trace!(target: LOG_TARGET, %peer_id, "Successfully polled peer");
                }
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        peer_id = %peer_id.to_short(),
                        err = %err,
                        "Error polling peer"
                    );
                    // On error, break and let the outer loop restart
                    break;
                }
            }
        }
    }

    async fn poll_once(
        conn: &Connection,
        db: &Database,
        self_id: RostraId,
        wot_rx: &watch::Receiver<Arc<WotData>>,
    ) -> Result<(), String> {
        // Call the blocking RPC
        let response = conn
            .wait_followers_new_heads()
            .await
            .map_err(|e| format!("RPC error: {}", e.fmt_compact()))?;

        let author = response.author;
        let event = response.event;

        let event_id = event.compute_short_id();
        trace!(
            target: LOG_TARGET,
            author = %author.to_short(),
            event_id = %event_id.to_short(),
            "Received new head event from peer"
        );

        // Verify the event is authentic
        let verified_event = VerifiedEvent::verify_received_as_is(event)
            .map_err(|e| format!("Event verification failed: {}", e.fmt_compact()))?;

        // Check if the author is in our web of trust
        let in_wot = {
            let wot = wot_rx.borrow();
            wot.contains(author, self_id)
        };

        if !in_wot {
            warn!(
                target: LOG_TARGET,
                author = %author.to_short(),
                "Received event from author not in web of trust, ignoring"
            );
            return Ok(());
        }

        // Store the event
        let (insert_outcome, process_state) = db.process_event(&verified_event).await;

        debug!(
            target: LOG_TARGET,
            author = %author.to_short(),
            event_id = %verified_event.event_id.to_short(),
            ?insert_outcome,
            ?process_state,
            "Processed new head event"
        );

        // Optionally fetch content if needed
        if db
            .wants_content(verified_event.event_id, process_state)
            .await
        {
            match conn.get_event_content(verified_event).await {
                Ok(Some(content)) => {
                    db.process_event_content(&content).await;
                    info!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        "Fetched and stored event content"
                    );
                }
                Ok(None) => {
                    debug!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        "Peer does not have event content"
                    );
                }
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        err = %err.fmt_compact(),
                        "Failed to fetch event content"
                    );
                }
            }
        }

        Ok(())
    }
}
