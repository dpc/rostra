use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use rostra_client_db::{Database, IdsFolloweesRecord};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tokio::sync::{RwLock, watch};
use tokio::time::Instant;
use tracing::{debug, instrument, trace, warn};

use crate::client::{Client, INITIAL_BACKOFF_DURATION, MAX_BACKOFF_DURATION};
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

const LOG_TARGET: &str = "rostra::poll_followee_heads";

/// Per-peer backoff state for polling.
#[derive(Debug, Clone, Default)]
struct PeerBackoffState {
    consecutive_failures: u32,
    backoff_until: Option<Instant>,
}

impl PeerBackoffState {
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

    fn is_in_backoff(&self) -> bool {
        self.backoff_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }

    fn backoff_remaining(&self) -> Option<Duration> {
        let until = self.backoff_until?;
        let now = Instant::now();
        if now < until { Some(until - now) } else { None }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.backoff_until = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let backoff_duration = self.calculate_backoff_duration();
        self.backoff_until = Some(Instant::now() + backoff_duration);
    }
}

type SharedBackoffState = Arc<RwLock<HashMap<RostraId, PeerBackoffState>>>;

/// Polls direct followees for head updates using the WAIT_HEAD_UPDATE RPC.
///
/// For each followee, connects and sends our current known head. The server
/// responds immediately if the head is stale, or waits until it changes.
/// This gives fast catch-up when reconnecting after being offline.
pub struct PollFolloweeHeadUpdates {
    client: crate::client::ClientHandle,
    networking: Arc<ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    self_followees_rx: watch::Receiver<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
    connections: ConnectionCache,
}

impl PollFolloweeHeadUpdates {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting poll followee head updates task");
        Self {
            client: client.handle(),
            networking: client.networking().clone(),
            db: client.db().clone(),
            self_id: client.rostra_id(),
            self_followees_rx: client.self_followees_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "poll-followee-head-updates", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(mut self) {
        let mut active_peers: HashSet<RostraId> = HashSet::new();
        let mut poll_futures: FuturesUnordered<_> = FuturesUnordered::new();
        let backoff_state: SharedBackoffState = Arc::new(RwLock::new(HashMap::new()));

        loop {
            // Get current active followees (selector.is_some() means active follow)
            let current_followees: HashSet<RostraId> = {
                let followees = self.self_followees_rx.borrow();
                followees
                    .iter()
                    .filter(|(_, record)| record.selector.is_some())
                    .map(|(id, _)| *id)
                    .collect()
            };

            // Add new followees
            let new_followees: Vec<_> = current_followees
                .difference(&active_peers)
                .copied()
                .collect();
            for followee_id in new_followees {
                debug!(target: LOG_TARGET, followee_id = %followee_id.to_short(), "Adding followee to poll list");
                active_peers.insert(followee_id);
            }

            // Spawn polling tasks for peers that need them
            let peers_to_poll: Vec<_> = active_peers.iter().copied().collect();
            for peer_id in peers_to_poll {
                let networking = self.networking.clone();
                let connections = self.connections.clone();
                let db = self.db.clone();
                let backoff = backoff_state.clone();

                poll_futures.push(async move {
                    Self::poll_followee(networking, connections, db, peer_id, backoff).await;
                    peer_id
                });
            }

            // Clear active peers - they'll be re-added based on poll results
            active_peers.clear();

            tokio::select! {
                Some(peer_id) = poll_futures.next() => {
                    trace!(target: LOG_TARGET, peer_id = %peer_id.to_short(), "Poll task completed, will restart");
                    active_peers.insert(peer_id);
                }
                res = self.self_followees_rx.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "Followees channel closed, shutting down");
                        break;
                    }
                    debug!(target: LOG_TARGET, "Followees changed, updating poll list");
                }
            }

            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            }
        }
    }

    async fn poll_followee(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        followee_id: RostraId,
        backoff_state: SharedBackoffState,
    ) {
        loop {
            // Check backoff
            {
                let state = backoff_state.read().await;
                if let Some(peer_state) = state.get(&followee_id) {
                    if peer_state.is_in_backoff() {
                        if let Some(remaining) = peer_state.backoff_remaining() {
                            trace!(
                                target: LOG_TARGET,
                                followee_id = %followee_id.to_short(),
                                remaining_secs = remaining.as_secs(),
                                "Followee is in backoff, waiting"
                            );
                            drop(state);
                            tokio::time::sleep(remaining).await;
                            continue;
                        }
                    }
                }
            }

            let conn = match connections.get_or_connect(&networking, followee_id).await {
                Ok(conn) => conn,
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        followee_id = %followee_id.to_short(),
                        err = %err.fmt_compact(),
                        "Could not connect to followee for polling"
                    );
                    let mut state = backoff_state.write().await;
                    let peer_state = state.entry(followee_id).or_default();
                    peer_state.record_failure();
                    debug!(
                        target: LOG_TARGET,
                        followee_id = %followee_id.to_short(),
                        consecutive_failures = peer_state.consecutive_failures,
                        backoff_secs = peer_state.calculate_backoff_duration().as_secs(),
                        "Connection failed, applying backoff"
                    );
                    continue;
                }
            };

            match Self::poll_once(&conn, &db, followee_id).await {
                Ok(()) => {
                    trace!(target: LOG_TARGET, followee_id = %followee_id.to_short(), "Successfully polled followee");
                    let mut state = backoff_state.write().await;
                    if let Some(peer_state) = state.get_mut(&followee_id) {
                        peer_state.record_success();
                    }
                }
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        followee_id = %followee_id.to_short(),
                        err = %err,
                        "Error polling followee"
                    );
                    let mut state = backoff_state.write().await;
                    let peer_state = state.entry(followee_id).or_default();
                    peer_state.record_failure();
                    debug!(
                        target: LOG_TARGET,
                        followee_id = %followee_id.to_short(),
                        consecutive_failures = peer_state.consecutive_failures,
                        backoff_secs = peer_state.calculate_backoff_duration().as_secs(),
                        "Poll failed, applying backoff"
                    );
                    break;
                }
            }
        }
    }

    async fn poll_once(
        conn: &rostra_p2p::Connection,
        db: &Database,
        followee_id: RostraId,
    ) -> Result<(), String> {
        // Get our current known head for this followee
        let known_heads = db.get_heads(followee_id).await;
        let known_head = known_heads
            .into_iter()
            .next()
            .unwrap_or(rostra_core::ShortEventId::ZERO);

        debug!(
            target: LOG_TARGET,
            followee_id = %followee_id.to_short(),
            known_head = %known_head.to_short(),
            "Waiting for head update from followee"
        );

        // Wait for head to change (responds immediately if stale)
        let new_head_id = conn
            .wait_head_update(known_head)
            .await
            .map_err(|e| format!("RPC error: {}", e.fmt_compact()))?;

        // If the peer returned the same head we sent, it's running the old
        // buggy handler (inverted logic). Back off to avoid a tight loop.
        if new_head_id == known_head {
            debug!(
                target: LOG_TARGET,
                followee_id = %followee_id.to_short(),
                head = %known_head.to_short(),
                "Peer returned same head (likely old handler bug), backing off"
            );
            tokio::time::sleep(Duration::from_secs(60)).await;
            return Ok(());
        }

        debug!(
            target: LOG_TARGET,
            followee_id = %followee_id.to_short(),
            new_head = %new_head_id.to_short(),
            "Received head update from followee"
        );

        // Fetch the full event
        let event = conn
            .get_event(followee_id, new_head_id)
            .await
            .map_err(|e| format!("Failed to fetch event: {}", e.fmt_compact()))?;

        let Some(verified_event) = event else {
            warn!(
                target: LOG_TARGET,
                followee_id = %followee_id.to_short(),
                new_head = %new_head_id.to_short(),
                "Followee reported head but event not found"
            );
            return Ok(());
        };

        // Store event without content (NewHeadFetcher will handle
        // content fetch and DAG traversal)
        let (insert_outcome, _process_state) = db.process_event(&verified_event).await;

        debug!(
            target: LOG_TARGET,
            followee_id = %followee_id.to_short(),
            event_id = %verified_event.event_id.to_short(),
            ?insert_outcome,
            "Stored followee head event (content deferred to NewHeadFetcher)"
        );

        Ok(())
    }
}
