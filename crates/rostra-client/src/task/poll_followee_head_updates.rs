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
const MISSING_EVENT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Remembers the last current head reported by one remote followee.
#[derive(Debug, Clone, Default)]
struct RemoteHeadCursor {
    head: Option<rostra_core::ShortEventId>,
}

impl RemoteHeadCursor {
    fn known_head(&self, local_head: rostra_core::ShortEventId) -> rostra_core::ShortEventId {
        self.head.unwrap_or(local_head)
    }

    fn update(&mut self, head: rostra_core::ShortEventId) {
        self.head = Some(head);
    }
}

/// Per-peer state for followee polling.
#[derive(Debug, Clone, Default)]
struct PeerPollState {
    consecutive_failures: u32,
    backoff_until: Option<Instant>,
    remote_head: RemoteHeadCursor,
    pending_event: Option<rostra_core::ShortEventId>,
}

impl PeerPollState {
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

type SharedPollState = Arc<RwLock<HashMap<RostraId, PeerPollState>>>;

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
        let poll_state: SharedPollState = Arc::new(RwLock::new(HashMap::new()));

        let _ = Self::update_desired_followees(
            &self.self_followees,
            &mut desired_peers,
            &active_peers,
            &mut pending_peers,
        );
        self.schedule_pending(
            &mut pending_peers,
            &mut active_peers,
            &mut poll_futures,
            &poll_state,
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
                    } else {
                        poll_state.write().await.remove(&peer_id);
                    }
                }
                res = self.self_followees.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "Followees channel closed, shutting down");
                        break;
                    }
                    debug!(target: LOG_TARGET, "Followees changed, updating poll list");
                    let removed_peers = Self::update_desired_followees(
                        &self.self_followees,
                        &mut desired_peers,
                        &active_peers,
                        &mut pending_peers,
                    );
                    let mut state = poll_state.write().await;
                    for peer_id in removed_peers {
                        state.remove(&peer_id);
                    }
                }
            }

            self.schedule_pending(
                &mut pending_peers,
                &mut active_peers,
                &mut poll_futures,
                &poll_state,
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
    ) -> BTreeSet<RostraId> {
        let previous_peers = desired_peers.clone();
        desired_peers.clear();
        desired_peers.extend(self_followees.snapshot().keys().copied());
        pending_peers.retain(|id| desired_peers.contains(id));

        for peer_id in desired_peers.difference(active_peers) {
            pending_peers.insert(*peer_id);
        }
        previous_peers.difference(desired_peers).copied().collect()
    }

    fn schedule_pending(
        &self,
        pending_peers: &mut BTreeSet<RostraId>,
        active_peers: &mut BTreeSet<RostraId>,
        poll_futures: &mut FuturesUnordered<BoxFuture<'static, (RostraId, DbResult<()>)>>,
        poll_state: &SharedPollState,
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
            let poll_state = poll_state.clone();
            poll_futures.push(Box::pin(async move {
                let result =
                    Self::poll_slot(networking, connections, db, peer_id, poll_state).await;
                (peer_id, result)
            }));
        }
    }

    async fn poll_slot(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        followee_id: RostraId,
        poll_state: SharedPollState,
    ) -> DbResult<()> {
        tokio::time::timeout(
            POLL_SLOT_TIMEOUT,
            Self::poll_followee(networking, connections, db, followee_id, poll_state),
        )
        .await
        .unwrap_or(Ok(()))
    }

    async fn poll_followee(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        followee_id: RostraId,
        poll_state: SharedPollState,
    ) -> DbResult<()> {
        loop {
            // Check backoff
            {
                let state = poll_state.read().await;
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
                    let mut state = poll_state.write().await;
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

            let Err(err) =
                Self::poll_connection_for_head_updates(&conn, &db, followee_id, &poll_state)
                    .await?
            else {
                unreachable!("a connected followee poll only returns after an RPC error");
            };
            debug!(
                target: LOG_TARGET,
                followee_id = %followee_id.to_short(),
                err = %err,
                "Error polling followee"
            );
            let mut state = poll_state.write().await;
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
        Ok(())
    }

    async fn poll_connection_for_head_updates(
        conn: &rostra_p2p::Connection,
        db: &Database,
        followee_id: RostraId,
        poll_state: &SharedPollState,
    ) -> DbResult<Result<(), String>> {
        let local_heads = db.get_heads(followee_id).await;
        let local_head =
            representative_head(&local_heads).unwrap_or(rostra_core::ShortEventId::ZERO);
        loop {
            let pending_event = poll_state
                .read()
                .await
                .get(&followee_id)
                .and_then(|state| state.pending_event);
            if let Some(event_id) = pending_event {
                tokio::time::sleep(MISSING_EVENT_RETRY_DELAY).await;
                let event = match conn.get_event(followee_id, event_id).await {
                    Ok(event) => event,
                    Err(err) => {
                        return Ok(Err(format!("Failed to fetch event: {}", err.fmt_compact())));
                    }
                };
                let Some(event) = event else {
                    continue;
                };
                let (insert_outcome, _process_state) = db.try_process_event(&event).await?;
                debug!(
                    target: LOG_TARGET,
                    followee_id = %followee_id.to_short(),
                    event_id = %event.event_id.to_short(),
                    ?insert_outcome,
                    "Stored retried followee head event (content deferred to NewHeadFetcher)"
                );
                let mut state = poll_state.write().await;
                let peer_state = state.entry(followee_id).or_default();
                if peer_state.pending_event == Some(event_id) {
                    peer_state.pending_event = None;
                }
                peer_state.record_success();
                continue;
            }
            let (_new_head, event) = match Self::poll_remote_head_update(
                conn,
                followee_id,
                local_head,
                poll_state,
            )
            .await
            {
                Ok(result) => result,
                Err(err) => return Ok(Err(err)),
            };
            if let Some(event) = event {
                let (insert_outcome, _process_state) = db.try_process_event(&event).await?;
                debug!(
                    target: LOG_TARGET,
                    followee_id = %followee_id.to_short(),
                    event_id = %event.event_id.to_short(),
                    ?insert_outcome,
                    "Stored followee head event (content deferred to NewHeadFetcher)"
                );
                poll_state
                    .write()
                    .await
                    .entry(followee_id)
                    .or_default()
                    .pending_event = None;
            }
            trace!(target: LOG_TARGET, followee_id = %followee_id.to_short(), "Successfully polled followee");
            let mut state = poll_state.write().await;
            let peer_state = state.entry(followee_id).or_default();
            peer_state.record_success();
        }
    }

    async fn poll_remote_head_update(
        conn: &rostra_p2p::Connection,
        followee_id: RostraId,
        local_head: rostra_core::ShortEventId,
        poll_state: &SharedPollState,
    ) -> Result<(rostra_core::ShortEventId, Option<VerifiedEvent>), String> {
        let known_head = {
            let state = poll_state.read().await;
            state
                .get(&followee_id)
                .map(|peer_state| peer_state.remote_head.known_head(local_head))
                .unwrap_or(local_head)
        };
        let (new_head, event) = Self::poll_once(conn, followee_id, known_head).await?;
        let mut state = poll_state.write().await;
        state
            .entry(followee_id)
            .or_default()
            .remote_head
            .update(new_head);
        state.entry(followee_id).or_default().pending_event = Some(new_head);
        Ok((new_head, event))
    }

    async fn poll_once(
        conn: &rostra_p2p::Connection,
        followee_id: RostraId,
        known_head: rostra_core::ShortEventId,
    ) -> Result<(rostra_core::ShortEventId, Option<VerifiedEvent>), String> {
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
            return Ok((new_head_id, None));
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
            return Ok((new_head_id, None));
        };

        Ok((new_head_id, Some(verified_event)))
    }
}

#[cfg(test)]
mod tests;
