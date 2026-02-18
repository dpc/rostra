use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use rostra_client_db::{Database, IdsFollowersRecord, WotData};
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::FmtCompact as _;
use tokio::sync::{RwLock, watch};
use tokio::time::Instant;
use tracing::{debug, instrument, trace, warn};

use crate::client::{Client, INITIAL_BACKOFF_DURATION, MAX_BACKOFF_DURATION};
use crate::connection_cache::ConnectionCache;

const LOG_TARGET: &str = "rostra::poll_follower_heads";

/// Per-peer backoff state for polling.
#[derive(Debug, Clone, Default)]
struct PeerBackoffState {
    /// Number of consecutive failures
    consecutive_failures: u32,
    /// Time until which we should not attempt to poll
    backoff_until: Option<Instant>,
}

impl PeerBackoffState {
    /// Calculate the backoff duration based on consecutive failures.
    fn calculate_backoff_duration(&self) -> Duration {
        if self.consecutive_failures == 0 {
            return Duration::ZERO;
        }
        let shift = self.consecutive_failures.saturating_sub(1).min(63);
        let multiplier = 1u64 << shift;
        let backoff_secs = INITIAL_BACKOFF_DURATION
            .as_secs()
            .saturating_mul(multiplier);
        Duration::from_secs(backoff_secs).min(MAX_BACKOFF_DURATION)
    }

    /// Check if we should skip polling due to backoff.
    fn is_in_backoff(&self) -> bool {
        self.backoff_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }

    /// Get remaining backoff duration, if any.
    fn backoff_remaining(&self) -> Option<Duration> {
        let until = self.backoff_until?;
        let now = Instant::now();
        if now < until { Some(until - now) } else { None }
    }

    /// Record a successful poll, resetting backoff state.
    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.backoff_until = None;
    }

    /// Record a failed poll, updating backoff state.
    fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let backoff_duration = self.calculate_backoff_duration();
        self.backoff_until = Some(Instant::now() + backoff_duration);
    }
}

/// Shared backoff state for all peers.
type SharedBackoffState = Arc<RwLock<HashMap<RostraId, PeerBackoffState>>>;

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
        // Shared backoff state for all peers
        let backoff_state: SharedBackoffState = Arc::new(RwLock::new(HashMap::new()));

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
                let backoff = backoff_state.clone();

                poll_futures.push(async move {
                    Self::poll_peer_for_heads(
                        client,
                        connections,
                        db,
                        self_id,
                        peer_id,
                        wot_rx,
                        backoff,
                    )
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
        backoff_state: SharedBackoffState,
    ) {
        loop {
            // Check if we're in backoff for this peer
            {
                let state = backoff_state.read().await;
                if let Some(peer_state) = state.get(&peer_id) {
                    if peer_state.is_in_backoff() {
                        if let Some(remaining) = peer_state.backoff_remaining() {
                            trace!(
                                target: LOG_TARGET,
                                peer_id = %peer_id.to_short(),
                                remaining_secs = remaining.as_secs(),
                                "Peer is in backoff, waiting"
                            );
                            // Sleep for the remaining backoff duration
                            drop(state); // Release lock before sleeping
                            tokio::time::sleep(remaining).await;
                            continue;
                        }
                    }
                }
            }

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
                    // Record failure and apply backoff
                    {
                        let mut state = backoff_state.write().await;
                        let peer_state = state.entry(peer_id).or_default();
                        peer_state.record_failure();
                        debug!(
                            target: LOG_TARGET,
                            peer_id = %peer_id.to_short(),
                            consecutive_failures = peer_state.consecutive_failures,
                            backoff_secs = peer_state.calculate_backoff_duration().as_secs(),
                            "Connection failed, applying backoff"
                        );
                    }
                    continue;
                }
            };

            match Self::poll_once(&conn, &db, self_id, &wot_rx).await {
                Ok(()) => {
                    trace!(target: LOG_TARGET, %peer_id, "Successfully polled peer");
                    // Reset backoff on success
                    {
                        let mut state = backoff_state.write().await;
                        if let Some(peer_state) = state.get_mut(&peer_id) {
                            peer_state.record_success();
                        }
                    }
                }
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        peer_id = %peer_id.to_short(),
                        err = %err,
                        "Error polling peer"
                    );
                    // Record failure and apply backoff
                    {
                        let mut state = backoff_state.write().await;
                        let peer_state = state.entry(peer_id).or_default();
                        peer_state.record_failure();
                        debug!(
                            target: LOG_TARGET,
                            peer_id = %peer_id.to_short(),
                            consecutive_failures = peer_state.consecutive_failures,
                            backoff_secs = peer_state.calculate_backoff_duration().as_secs(),
                            "Poll failed, applying backoff"
                        );
                    }
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

        // Store the event (without content). Content will be fetched by
        // NewHeadFetcher via download_events_from_child, which also
        // traverses parent events. Not fetching content here ensures that
        // wants_content() returns true for this event, preventing the
        // probabilistic cutoff in download_events_from_child from skipping
        // parent traversal.
        let (insert_outcome, _process_state) = db.process_event(&verified_event).await;

        debug!(
            target: LOG_TARGET,
            author = %author.to_short(),
            event_id = %verified_event.event_id.to_short(),
            ?insert_outcome,
            "Stored new head event (content deferred to NewHeadFetcher)"
        );

        Ok(())
    }
}
