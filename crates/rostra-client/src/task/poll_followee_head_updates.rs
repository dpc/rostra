use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use rostra_client_db::{CurrentState, Database, DbResult, IdsFolloweesRecord};
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{debug, error, instrument, trace, warn};

use crate::client::{Client, INITIAL_BACKOFF_DURATION, MAX_BACKOFF_DURATION};
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;
use crate::task::head_selection::representative_head;

const LOG_TARGET: &str = "rostra::poll_followee_heads";
const MAX_ACTIVE_POLLS: usize = 32;
const POLL_SLOT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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
/// responds immediately if that head is stale, or waits until it stops being a
/// current head. An undiscovered sibling does not complete this legacy
/// single-head wait while the known head remains current. Periodic sampled
/// `GET_HEAD` discovery complements this fast update path.
pub struct PollFolloweeHeadUpdates {
    client: crate::client::ClientHandle,
    networking: Arc<ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    self_followees: CurrentState<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
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
            self_followees: client.self_followees_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "poll-followee-head-updates", skip(self), fields(self_id = %self.self_id.fmt_short()), ret)]
    pub async fn run(mut self) {
        let mut desired_peers = BTreeSet::new();
        let mut pending_peers = BTreeSet::new();
        let mut active_peers = BTreeSet::new();
        let mut poll_futures = FuturesUnordered::new();
        let backoff_state: SharedBackoffState = Arc::new(RwLock::new(HashMap::new()));

        Self::update_desired_followees(
            &self.self_followees,
            &mut desired_peers,
            &active_peers,
            &mut pending_peers,
        );
        self.schedule_pending(
            &mut pending_peers,
            &mut active_peers,
            &mut poll_futures,
            &backoff_state,
        );

        loop {
            tokio::select! {
                Some((peer_id, result)) = poll_futures.next() => {
                    active_peers.remove(&peer_id);
                    if let Err(err) = result {
                        error!(
                            target: LOG_TARGET,
                            followee_id = %peer_id.to_short(),
                            err = %err,
                            "Failed to store a polled followee head; stopping poll task"
                        );
                        return;
                    }
                    trace!(target: LOG_TARGET, peer_id = %peer_id.to_short(), "Poll task completed");
                    if desired_peers.contains(&peer_id) {
                        pending_peers.insert(peer_id);
                    }
                }
                res = self.self_followees.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "Followees channel closed, shutting down");
                        break;
                    }
                    debug!(target: LOG_TARGET, "Followees changed, updating poll list");
                    Self::update_desired_followees(
                        &self.self_followees,
                        &mut desired_peers,
                        &active_peers,
                        &mut pending_peers,
                    );
                }
            }

            self.schedule_pending(
                &mut pending_peers,
                &mut active_peers,
                &mut poll_futures,
                &backoff_state,
            );

            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            }
        }
    }

    fn update_desired_followees(
        self_followees: &CurrentState<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
        desired_peers: &mut BTreeSet<RostraId>,
        active_peers: &BTreeSet<RostraId>,
        pending_peers: &mut BTreeSet<RostraId>,
    ) {
        desired_peers.clear();
        desired_peers.extend(self_followees.snapshot().keys().copied());
        pending_peers.retain(|id| desired_peers.contains(id));

        for peer_id in desired_peers.difference(active_peers) {
            pending_peers.insert(*peer_id);
        }
    }

    fn schedule_pending(
        &self,
        pending_peers: &mut BTreeSet<RostraId>,
        active_peers: &mut BTreeSet<RostraId>,
        poll_futures: &mut FuturesUnordered<BoxFuture<'static, (RostraId, DbResult<()>)>>,
        backoff_state: &SharedBackoffState,
    ) {
        while active_peers.len() < MAX_ACTIVE_POLLS {
            let Some(peer_id) = pending_peers.pop_first() else {
                break;
            };
            if !active_peers.insert(peer_id) {
                continue;
            }

            let networking = self.networking.clone();
            let connections = self.connections.clone();
            let db = self.db.clone();
            let backoff = backoff_state.clone();
            poll_futures.push(Box::pin(async move {
                let result = tokio::time::timeout(
                    POLL_SLOT_TIMEOUT,
                    Self::poll_followee(networking, connections, db, peer_id, backoff),
                )
                .await
                .unwrap_or(Ok(()));
                (peer_id, result)
            }));
        }
    }

    async fn poll_followee(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        followee_id: RostraId,
        backoff_state: SharedBackoffState,
    ) -> DbResult<()> {
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
                Ok(event) => {
                    if let Some(event) = event {
                        let (insert_outcome, _process_state) = db.try_process_event(&event).await?;
                        debug!(
                            target: LOG_TARGET,
                            followee_id = %followee_id.to_short(),
                            event_id = %event.event_id.to_short(),
                            ?insert_outcome,
                            "Stored followee head event (content deferred to NewHeadFetcher)"
                        );
                    }
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
        Ok(())
    }

    async fn poll_once(
        conn: &rostra_p2p::Connection,
        db: &Database,
        followee_id: RostraId,
    ) -> Result<Option<VerifiedEvent>, String> {
        // Get our current known head for this followee
        let known_heads = db.get_heads(followee_id).await;
        let known_head =
            representative_head(&known_heads).unwrap_or(rostra_core::ShortEventId::ZERO);

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
            return Ok(None);
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
            return Ok(None);
        };

        Ok(Some(verified_event))
    }
}
