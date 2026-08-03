use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use rostra_client_db::{CurrentState, Database, DbResult, IdsFollowersRecord, WotData};
use rostra_core::event::{EventExt as _, VerifiedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::FmtCompact as _;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{debug, error, instrument, trace, warn};

use crate::client::{Client, INITIAL_BACKOFF_DURATION, MAX_BACKOFF_DURATION};
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

const LOG_TARGET: &str = "rostra::poll_follower_heads";
const MAX_ACTIVE_POLLS: usize = 32;
const POLL_SLOT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const NO_PROGRESS_POLL_DELAY: Duration = Duration::from_secs(1);

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

/// Classifies whether a successful follower poll changed local event state.
#[derive(Debug)]
enum PollProgress {
    Inserted(rostra_client_db::InsertEventOutcome),
    NoProgress,
}

/// Polls followers for new head updates using the WAIT_FOLLOWERS_NEW_HEADS RPC.
///
/// This task maintains connections to self and direct followers, polling each
/// for head updates using a blocking RPC call. When a new head is discovered,
/// the event is verified, bound to the response's claimed author, and added to
/// the database only when that authenticated author is in the local Web of
/// Trust.
pub struct PollFollowerHeadUpdates {
    client: crate::client::ClientHandle,
    networking: Arc<ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    self_followers: CurrentState<Arc<HashMap<RostraId, IdsFollowersRecord>>>,
    self_wot: CurrentState<Arc<WotData>>,
    connections: ConnectionCache,
}

impl PollFollowerHeadUpdates {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting poll follower head updates task");
        Self {
            client: client.handle(),
            networking: client.networking().clone(),
            db: client.db().clone(),
            self_id: client.rostra_id(),
            self_followers: client.self_followers_subscribe(),
            self_wot: client.self_wot_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "poll-follower-head-updates", skip(self), fields(self_id = %self.self_id.fmt_short()), ret)]
    pub async fn run(mut self) {
        let mut desired_peers = BTreeSet::new();
        let mut pending_peers = BTreeSet::new();
        let mut active_peers = BTreeSet::new();
        let mut poll_futures = FuturesUnordered::new();
        let backoff_state: SharedBackoffState = Arc::new(RwLock::new(HashMap::new()));

        self.update_desired_followers(&mut desired_peers, &active_peers, &mut pending_peers);
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
                            %peer_id,
                            err = %err,
                            "Failed to store a polled follower head; stopping poll task"
                        );
                        return;
                    }
                    trace!(target: LOG_TARGET, %peer_id, "Poll task completed");
                    if desired_peers.contains(&peer_id) {
                        pending_peers.insert(peer_id);
                    }
                }
                res = self.self_followers.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "Followers channel closed, shutting down");
                        break;
                    }
                    debug!(target: LOG_TARGET, "Followers changed, updating poll list");
                    self.update_desired_followers(
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

    fn update_desired_followers(
        &self,
        desired_peers: &mut BTreeSet<RostraId>,
        active_peers: &BTreeSet<RostraId>,
        pending_peers: &mut BTreeSet<RostraId>,
    ) {
        desired_peers.clear();
        desired_peers.insert(self.self_id);
        desired_peers.extend(self.self_followers.snapshot().keys().copied());
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
            let self_id = self.self_id;
            let wot = self.self_wot.clone();
            let backoff = backoff_state.clone();
            poll_futures.push(Box::pin(async move {
                let result = tokio::time::timeout(
                    POLL_SLOT_TIMEOUT,
                    Self::poll_peer_for_heads(
                        networking,
                        connections,
                        db,
                        self_id,
                        peer_id,
                        wot,
                        backoff,
                    ),
                )
                .await
                .unwrap_or(Ok(()));
                (peer_id, result)
            }));
        }
    }

    async fn poll_peer_for_heads(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        self_id: RostraId,
        peer_id: RostraId,
        wot: CurrentState<Arc<WotData>>,
        backoff_state: SharedBackoffState,
    ) -> DbResult<()> {
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

            let conn = match connections.get_or_connect(&networking, peer_id).await {
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

            let Err(err) =
                Self::poll_connection_for_heads(&conn, &db, self_id, peer_id, &wot, &backoff_state)
                    .await?
            else {
                unreachable!("a connected follower poll only returns after an RPC error");
            };
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
        Ok(())
    }

    async fn poll_connection_for_heads(
        conn: &Connection,
        db: &Database,
        self_id: RostraId,
        peer_id: RostraId,
        wot: &CurrentState<Arc<WotData>>,
        backoff_state: &SharedBackoffState,
    ) -> DbResult<Result<(), String>> {
        loop {
            match Self::poll_once(conn, self_id, wot).await {
                Ok(event) => {
                    match Self::finish_successful_poll(db, peer_id, event.as_ref(), backoff_state)
                        .await?
                    {
                        PollProgress::Inserted(insert_outcome) => {
                            let event = event.as_ref().expect("insert outcome requires an event");
                            debug!(
                                target: LOG_TARGET,
                                author = %event.author().to_short(),
                                event_id = %event.event_id.to_short(),
                                ?insert_outcome,
                                "Stored new head event (content deferred to NewHeadFetcher)"
                            );
                        }
                        PollProgress::NoProgress => {
                            debug!(
                                target: LOG_TARGET,
                                peer_id = %peer_id.to_short(),
                                delay_secs = NO_PROGRESS_POLL_DELAY.as_secs(),
                                "Successful follower poll made no progress, delaying retry"
                            );
                            tokio::time::sleep(NO_PROGRESS_POLL_DELAY).await;
                        }
                    }
                    trace!(target: LOG_TARGET, %peer_id, "Successfully polled peer");
                }
                Err(err) => {
                    return Ok(Err(err));
                }
            }
        }
    }

    async fn finish_successful_poll(
        db: &Database,
        peer_id: RostraId,
        event: Option<&VerifiedEvent>,
        backoff_state: &SharedBackoffState,
    ) -> DbResult<PollProgress> {
        let progress = if let Some(event) = event {
            let (insert_outcome, _process_state) = match db.try_process_event(event).await {
                Ok(outcome) => outcome,
                Err(err) => {
                    error!(
                        target: LOG_TARGET,
                        peer_id = %peer_id.to_short(),
                        author = %event.author().to_short(),
                        event_id = %event.event_id.to_short(),
                        err = %err,
                        "Failed to store a head event received from a follower"
                    );
                    return Err(err);
                }
            };
            match insert_outcome {
                rostra_client_db::InsertEventOutcome::Inserted { .. } => {
                    PollProgress::Inserted(insert_outcome)
                }
                rostra_client_db::InsertEventOutcome::AlreadyPresent => PollProgress::NoProgress,
            }
        } else {
            PollProgress::NoProgress
        };

        let mut state = backoff_state.write().await;
        if let Some(peer_state) = state.get_mut(&peer_id) {
            peer_state.record_success();
        }
        Ok(progress)
    }

    async fn poll_once(
        conn: &Connection,
        self_id: RostraId,
        wot: &CurrentState<Arc<WotData>>,
    ) -> Result<Option<VerifiedEvent>, String> {
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
            claimed_author = %author.to_short(),
            event_id = %event_id.to_short(),
            "Received new head event from peer"
        );

        // Bind the routing/admission claim to the signed envelope. The response
        // author is peer-controlled and must not independently grant WoT
        // admission to an event signed by another identity.
        let verified_event = VerifiedEvent::verify_signed(author, event)
            .map_err(|e| format!("Event verification failed: {}", e.fmt_compact()))?;
        let authenticated_author = verified_event.author();

        // Check the cryptographically authenticated author against our Web of
        // Trust only after the response binding has succeeded.
        let in_wot = { wot.snapshot().contains(authenticated_author, self_id) };

        if !in_wot {
            warn!(
                target: LOG_TARGET,
                author = %authenticated_author.to_short(),
                "Received event from author not in web of trust, ignoring"
            );
            return Ok(None);
        }

        // The caller stores the envelope after separating storage failure from
        // transient peer errors. Content remains deferred to NewHeadFetcher.
        Ok(Some(verified_event))
    }
}

#[cfg(test)]
mod tests;
