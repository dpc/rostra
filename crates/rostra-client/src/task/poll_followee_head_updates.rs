use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use rostra_client_db::{CurrentState, Database, DbError, DbResult, IdsFolloweesRecord};
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tokio::sync::{RwLock, oneshot};
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

/// Remote progress retained for one uninterrupted follow epoch.
///
/// A new remote head becomes `Pending` before any cancellable fetch or
/// persistence await. It becomes `Persisted` only after durable ingestion, so
/// cancellation always leaves enough state to retry without repeating `WAIT`.
#[derive(Debug, Clone, Copy, Default)]
enum RemoteProgress {
    #[default]
    Unknown,
    Pending(rostra_core::ShortEventId),
    Persisted(rostra_core::ShortEventId),
}

/// Per-peer state for one uninterrupted followee polling epoch.
///
/// The scheduler discards this state when the follow epoch changes.
#[derive(Debug, Clone, Default)]
struct FolloweePollState {
    consecutive_failures: u32,
    backoff_until: Option<Instant>,
    remote_progress: RemoteProgress,
}

impl FolloweePollState {
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

    fn wait_cursor(&self, local_head: rostra_core::ShortEventId) -> rostra_core::ShortEventId {
        match self.remote_progress {
            RemoteProgress::Unknown => local_head,
            RemoteProgress::Pending(head) | RemoteProgress::Persisted(head) => head,
        }
    }

    fn pending_event(&self) -> Option<rostra_core::ShortEventId> {
        match self.remote_progress {
            RemoteProgress::Pending(event_id) => Some(event_id),
            RemoteProgress::Unknown | RemoteProgress::Persisted(_) => None,
        }
    }

    fn record_remote_head(&mut self, head: rostra_core::ShortEventId) {
        self.remote_progress = RemoteProgress::Pending(head);
    }

    fn complete_pending_event(&mut self, event_id: rostra_core::ShortEventId) {
        if matches!(self.remote_progress, RemoteProgress::Pending(pending) if pending == event_id) {
            self.remote_progress = RemoteProgress::Persisted(event_id);
        }
    }
}

type FolloweeState = Arc<RwLock<FolloweePollState>>;
type FolloweeStates = HashMap<RostraId, FolloweeState>;
type FollowEpoch = rostra_core::ShortEventId;

struct ActiveFolloweePoll {
    epoch: FollowEpoch,
    cancel: oneshot::Sender<()>,
}

#[derive(Debug)]
enum FolloweePollError {
    Peer(rostra_p2p::RpcError),
    Database(DbError),
}

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
        let mut desired_peers = HashMap::new();
        let mut pending_peers = std::collections::BTreeMap::new();
        let mut active_peers = HashMap::new();
        let mut poll_futures = FuturesUnordered::new();
        let mut followee_states = HashMap::new();

        Self::update_desired_followees(
            &self.self_followees,
            &mut desired_peers,
            &mut pending_peers,
            &mut active_peers,
            &mut followee_states,
        );
        self.schedule_pending(
            &mut pending_peers,
            &mut active_peers,
            &mut poll_futures,
            &mut followee_states,
        );

        loop {
            tokio::select! {
                Some((peer_id, epoch, result)) = poll_futures.next() => {
                    if active_peers.get(&peer_id).is_some_and(|active| active.epoch == epoch) {
                        active_peers.remove(&peer_id);
                    }
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
                    if desired_peers.get(&peer_id) == Some(&epoch) {
                        pending_peers.insert(peer_id, epoch);
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
                        &mut pending_peers,
                        &mut active_peers,
                        &mut followee_states,
                    );
                }
            }

            self.schedule_pending(
                &mut pending_peers,
                &mut active_peers,
                &mut poll_futures,
                &mut followee_states,
            );

            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            }
        }
    }

    fn update_desired_followees(
        self_followees: &CurrentState<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
        desired_peers: &mut HashMap<RostraId, FollowEpoch>,
        pending_peers: &mut std::collections::BTreeMap<RostraId, FollowEpoch>,
        active_peers: &mut HashMap<RostraId, ActiveFolloweePoll>,
        followee_states: &mut FolloweeStates,
    ) {
        let new_desired: HashMap<_, _> = self_followees
            .snapshot()
            .iter()
            .map(|(peer_id, record)| (*peer_id, record.latest_event_id))
            .collect();
        Self::reconcile_followee_epochs(
            new_desired,
            desired_peers,
            pending_peers,
            active_peers,
            followee_states,
        );
    }

    fn reconcile_followee_epochs(
        new_desired: HashMap<RostraId, FollowEpoch>,
        desired_peers: &mut HashMap<RostraId, FollowEpoch>,
        pending_peers: &mut std::collections::BTreeMap<RostraId, FollowEpoch>,
        active_peers: &mut HashMap<RostraId, ActiveFolloweePoll>,
        followee_states: &mut FolloweeStates,
    ) {
        let retired: Vec<_> = active_peers
            .iter()
            .filter_map(|(peer_id, active)| {
                (new_desired.get(peer_id) != Some(&active.epoch)).then_some(*peer_id)
            })
            .collect();
        for peer_id in retired {
            if let Some(active) = active_peers.remove(&peer_id) {
                let _ = active.cancel.send(());
            }
        }
        pending_peers.retain(|peer_id, epoch| new_desired.get(peer_id) == Some(epoch));
        followee_states.retain(|peer_id, _| desired_peers.get(peer_id) == new_desired.get(peer_id));
        for (peer_id, epoch) in &new_desired {
            if !active_peers.contains_key(peer_id) {
                pending_peers.insert(*peer_id, *epoch);
            }
        }
        *desired_peers = new_desired;
    }

    fn schedule_pending(
        &self,
        pending_peers: &mut std::collections::BTreeMap<RostraId, FollowEpoch>,
        active_peers: &mut HashMap<RostraId, ActiveFolloweePoll>,
        poll_futures: &mut FuturesUnordered<
            BoxFuture<'static, (RostraId, FollowEpoch, DbResult<()>)>,
        >,
        followee_states: &mut FolloweeStates,
    ) {
        while active_peers.len() < MAX_ACTIVE_POLLS {
            let Some((peer_id, epoch)) = pending_peers.pop_first() else {
                break;
            };
            if active_peers.contains_key(&peer_id) {
                continue;
            }

            let networking = self.networking.clone();
            let connections = self.connections.clone();
            let db = self.db.clone();
            let followee_state = followee_states.entry(peer_id).or_default().clone();
            let (cancel, cancelled) = oneshot::channel();
            active_peers.insert(peer_id, ActiveFolloweePoll { epoch, cancel });
            poll_futures.push(Box::pin(async move {
                let result = tokio::select! {
                    result = Self::poll_slot(networking, connections, db, peer_id, followee_state) => result,
                    _ = cancelled => Ok(()),
                };
                (peer_id, epoch, result)
            }));
        }
    }

    async fn poll_slot(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        followee_id: RostraId,
        followee_state: FolloweeState,
    ) -> DbResult<()> {
        tokio::time::timeout(
            POLL_SLOT_TIMEOUT,
            Self::poll_followee(networking, connections, db, followee_id, followee_state),
        )
        .await
        .unwrap_or(Ok(()))
    }

    async fn poll_followee(
        networking: Arc<ClientNetworking>,
        connections: ConnectionCache,
        db: Arc<Database>,
        followee_id: RostraId,
        followee_state: FolloweeState,
    ) -> DbResult<()> {
        loop {
            // Check backoff
            {
                let state = followee_state.read().await;
                if state.is_in_backoff() {
                    if let Some(remaining) = state.backoff_remaining() {
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

            let conn = match connections.get_or_connect(&networking, followee_id).await {
                Ok(conn) => conn,
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        followee_id = %followee_id.to_short(),
                        err = %err.fmt_compact(),
                        "Could not connect to followee for polling"
                    );
                    let mut state = followee_state.write().await;
                    state.record_failure();
                    debug!(
                        target: LOG_TARGET,
                        followee_id = %followee_id.to_short(),
                        consecutive_failures = state.consecutive_failures,
                        backoff_secs = state.calculate_backoff_duration().as_secs(),
                        "Connection failed, applying backoff"
                    );
                    continue;
                }
            };

            let local_heads = db.get_heads(followee_id).await;
            let local_head =
                representative_head(&local_heads).unwrap_or(rostra_core::ShortEventId::ZERO);
            let err = match Self::poll_connection_slot(
                &conn,
                followee_id,
                local_head,
                &followee_state,
                |event| {
                    let db = db.clone();
                    async move {
                        let (insert_outcome, _process_state) = db.try_process_event(&event).await?;
                        debug!(
                            target: LOG_TARGET,
                            followee_id = %followee_id.to_short(),
                            event_id = %event.event_id.to_short(),
                            ?insert_outcome,
                            "Stored followee head event (content deferred to NewHeadFetcher)"
                        );
                        Ok(())
                    }
                },
            )
            .await
            {
                Err(FolloweePollError::Peer(err)) => err,
                Err(FolloweePollError::Database(err)) => return Err(err),
                Ok(()) => unreachable!("a connected followee slot runs until an RPC error"),
            };
            debug!(
                target: LOG_TARGET,
                followee_id = %followee_id.to_short(),
                err = %err,
                "Error polling followee"
            );
            let mut state = followee_state.write().await;
            state.record_failure();
            debug!(
                target: LOG_TARGET,
                followee_id = %followee_id.to_short(),
                consecutive_failures = state.consecutive_failures,
                backoff_secs = state.calculate_backoff_duration().as_secs(),
                "Poll failed, applying backoff"
            );
            break;
        }
        Ok(())
    }

    async fn poll_connection_slot<F, Fut>(
        conn: &rostra_p2p::Connection,
        followee_id: RostraId,
        local_head: rostra_core::ShortEventId,
        followee_state: &RwLock<FolloweePollState>,
        mut persist_event: F,
    ) -> Result<(), FolloweePollError>
    where
        F: FnMut(VerifiedEvent) -> Fut,
        Fut: Future<Output = DbResult<()>>,
    {
        loop {
            let pending_event = followee_state.read().await.pending_event();
            if let Some(event_id) = pending_event {
                tokio::time::sleep(MISSING_EVENT_RETRY_DELAY).await;
                let Some(event) = Self::fetch_pending_event(conn, followee_id, event_id).await?
                else {
                    continue;
                };
                persist_event(event)
                    .await
                    .map_err(FolloweePollError::Database)?;
                let mut state = followee_state.write().await;
                state.complete_pending_event(event_id);
                state.record_success();
                continue;
            }
            let known_head = followee_state.read().await.wait_cursor(local_head);
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
                .map_err(FolloweePollError::Peer)?;

            if new_head_id == known_head {
                debug!(
                    target: LOG_TARGET,
                    followee_id = %followee_id.to_short(),
                    head = %known_head.to_short(),
                    "Peer returned same head (likely old handler bug), backing off"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }

            debug!(
                target: LOG_TARGET,
                followee_id = %followee_id.to_short(),
                new_head = %new_head_id.to_short(),
                "Received head update from followee"
            );

            followee_state.write().await.record_remote_head(new_head_id);
            let Some(event) = Self::fetch_pending_event(conn, followee_id, new_head_id).await?
            else {
                warn!(
                    target: LOG_TARGET,
                    followee_id = %followee_id.to_short(),
                    new_head = %new_head_id.to_short(),
                    "Followee reported head but event not found"
                );
                continue;
            };
            persist_event(event)
                .await
                .map_err(FolloweePollError::Database)?;
            let mut state = followee_state.write().await;
            state.complete_pending_event(new_head_id);
            state.record_success();
            trace!(target: LOG_TARGET, followee_id = %followee_id.to_short(), "Successfully polled followee");
        }
    }

    async fn fetch_pending_event(
        conn: &rostra_p2p::Connection,
        followee_id: RostraId,
        event_id: rostra_core::ShortEventId,
    ) -> Result<Option<VerifiedEvent>, FolloweePollError> {
        conn.get_event(followee_id, event_id)
            .await
            .map_err(FolloweePollError::Peer)
    }
}

#[cfg(test)]
mod tests;
