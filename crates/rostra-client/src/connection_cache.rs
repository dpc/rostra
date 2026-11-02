use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::stream::{self, StreamExt as _};
use rostra_core::ShortEventId;
use rostra_core::event::{EventExt as _, VerifiedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, trace};

use crate::error::ConnectResult;
use crate::holder_hints::{HolderHint, HolderHints};
use crate::net::ClientNetworking;

/// A normal account route or one bounded, untrusted holder attempt.
#[derive(Clone, Copy)]
enum FetchPeer {
    Account(RostraId),
    Holder,
}

/// Keep an ordinary route first, including when payload capacity allows one
/// read.
fn fetch_peers(peers: &[RostraId]) -> Vec<FetchPeer> {
    let mut result: Vec<_> = peers.iter().copied().map(FetchPeer::Account).collect();
    result.insert(result.len().min(1), FetchPeer::Holder);
    result
}

const LOG_TARGET: &str = "rostra-client::connection-cache";
const CLEANUP_INTERVAL: u64 = 64;

type LazySharedConnection = Arc<OnceCell<Connection>>;

async fn fetch_headers(
    conn: &Connection,
    author: RostraId,
    root: ShortEventId,
    ancestors: bool,
) -> Result<Option<Vec<VerifiedEvent>>, rostra_p2p::RpcError> {
    if ancestors && let Some(page) = conn.get_ancestors(author, root).await {
        return Ok((!page.is_empty()).then_some(page));
    }
    Ok(conn.get_event(author, root).await?.map(|event| vec![event]))
}

/// Detailed content acquisition result for storage-progress accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentFetchOutcome {
    /// No further acquisition is needed or downloaded ingestion succeeded.
    pub(crate) acquisition_satisfied: bool,
    /// This invocation transactionally materialized the event content.
    pub(crate) materialized: bool,
}

impl ContentFetchOutcome {
    fn from_ingest(outcome: rostra_client_db::PayloadIngestOutcome) -> Self {
        use rostra_client_db::PayloadIngestOutcome;

        match outcome {
            PayloadIngestOutcome::Processed => Self {
                acquisition_satisfied: true,
                materialized: true,
            },
            PayloadIngestOutcome::Invalid | PayloadIngestOutcome::Unchanged => Self {
                acquisition_satisfied: true,
                materialized: false,
            },
            PayloadIngestOutcome::Unavailable => Self {
                acquisition_satisfied: false,
                materialized: false,
            },
            PayloadIngestOutcome::Deferred(_) => {
                unreachable!("AcquiredPayload converts deferred ingestion into an error")
            }
        }
    }
}

#[derive(Clone)]
pub struct ConnectionCache {
    connections: Arc<Mutex<HashMap<RostraId, LazySharedConnection>>>,
    access_count: Arc<AtomicU64>,
    peer_operation_deadline: Duration,
    /// Untrusted content routes, separate from the account connection cache.
    pub(crate) holders: Arc<HolderHints>,
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
            access_count: Arc::new(AtomicU64::new(0)),
            peer_operation_deadline: crate::task::outbound_deadline::PEER_OPERATION_DEADLINE,
            holders: Arc::new(HolderHints::new()),
        }
    }

    #[cfg(test)]
    fn with_peer_operation_deadline(peer_operation_deadline: Duration) -> Self {
        Self {
            peer_operation_deadline,
            ..Self::new()
        }
    }

    fn maybe_cleanup_closed(&self, connections: &mut HashMap<RostraId, LazySharedConnection>) {
        let removed = self.maybe_cleanup_cells(connections, |connection| !connection.is_closed());
        if 0 < removed {
            trace!(
                target: LOG_TARGET,
                removed,
                remaining = connections.len(),
                "Removed closed or abandoned connections from cache"
            );
        }
    }

    fn maybe_cleanup_cells<K, T>(
        &self,
        cells: &mut HashMap<K, Arc<OnceCell<T>>>,
        is_open: impl Fn(&T) -> bool,
    ) -> usize
    where
        K: Eq + Hash,
    {
        self.maybe_cleanup_cells_after_shared(cells, is_open, |_| {})
    }

    fn maybe_cleanup_cells_after_shared<K, T>(
        &self,
        cells: &mut HashMap<K, Arc<OnceCell<T>>>,
        is_open: impl Fn(&T) -> bool,
        mut after_shared_detected: impl FnMut(&K),
    ) -> usize
    where
        K: Eq + Hash,
    {
        let access_count = self.access_count.fetch_add(1, Ordering::Relaxed);
        if !access_count.is_multiple_of(CLEANUP_INTERVAL) {
            return 0;
        }

        let before = cells.len();
        cells.retain(|key, cell| {
            if let Some(cell) = Arc::get_mut(cell) {
                cell.get().is_some_and(&is_open)
            } else {
                after_shared_detected(key);
                cell.get().is_none_or(&is_open)
            }
        });
        before.saturating_sub(cells.len())
    }

    pub async fn get_or_connect(
        &self,
        networking: &ClientNetworking,
        id: RostraId,
    ) -> ConnectResult<Connection> {
        let mut pool_lock = self.connections.lock().await;
        self.maybe_cleanup_closed(&mut pool_lock);

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
                        debug!(target: LOG_TARGET, %id, endpoint_id = %conn.remote_id().fmt_short(), "Connection successful");
                        Ok(conn)
                    }
                    Err(err) => {
                        trace!(target: LOG_TARGET, %id, err = %err, "Connection failed");
                        Err(err)
                    }
                }
            })
            .await;

        result.cloned()
    }

    async fn is_connected(&self, id: RostraId) -> bool {
        self.connections
            .lock()
            .await
            .get(&id)
            .and_then(|connection| connection.get())
            .is_some_and(|connection| !connection.is_closed())
    }

    /// Try to fetch an event from multiple peers with some parallelism.
    ///
    /// Returns `Some(event)` from the first peer that has it, or `None`.
    #[cfg(test)]
    pub async fn get_event_from_peers(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        author_id: RostraId,
        event_id: ShortEventId,
    ) -> Option<VerifiedEvent> {
        self.get_event_with_source(networking, peers, author_id, event_id)
            .await
            .map(|(event, _)| event)
    }

    /// Fetch a verified envelope and preserve a holder deadline for
    /// post-admission propagation.
    pub(crate) async fn get_event_with_source(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        author_id: RostraId,
        event_id: ShortEventId,
    ) -> Option<(VerifiedEvent, Option<HolderHint>)> {
        self.get_headers_with_source(networking, peers, author_id, event_id, false)
            .await
            .map(|(events, source)| (events[0], source))
    }

    /// Race bounded header lookahead, preserving only the actual winning
    /// source.
    pub(crate) async fn get_headers_with_source(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        author_id: RostraId,
        event_id: ShortEventId,
        ancestors: bool,
    ) -> Option<(Vec<VerifiedEvent>, Option<HolderHint>)> {
        let result = futures_lite::StreamExt::find_map(
            &mut stream::iter(fetch_peers(peers))
                .map(|peer| {
                    let cache = self.clone();
                    async move {
                        let hint = if matches!(peer, FetchPeer::Holder) {
                            let _permit = cache.holders.operations.try_acquire().ok()?;
                            let attempt = cache.holders.attempt(author_id, event_id)?;
                            let hint = attempt.hint;
                            let event = tokio::time::timeout(
                                hint.timeout().min(cache.peer_operation_deadline),
                                async {
                                    let conn = cache.connect_holder(networking, hint).await?;
                                    // Holder connections are short-lived, not capability-cached.
                                    // Keep their existing single-header path rather than probing
                                    // an old holder again for every generation.
                                    fetch_headers(&conn, author_id, event_id, false)
                                        .await
                                        .ok()
                                        .flatten()
                                },
                            )
                            .await
                            .ok()
                            .flatten()?;
                            attempt.succeeded();
                            return Some((event, Some(hint)));
                        } else {
                            None
                        };
                        let FetchPeer::Account(peer_id) = peer else {
                            unreachable!()
                        };
                        crate::task::outbound_deadline::within(
                            cache.peer_operation_deadline,
                            async move {
                                let conn = cache.get_or_connect(networking, peer_id).await.ok()?;
                                match fetch_headers(&conn, author_id, event_id, ancestors).await {
                                    Ok(Some(event)) => Some((event, hint)),
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
                            },
                        )
                        .await
                        .ok()
                        .flatten()
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

    /// Redial only an authenticated endpoint ID using configured
    /// discovery/privacy policy.
    async fn connect_holder(
        &self,
        networking: &ClientNetworking,
        hint: HolderHint,
    ) -> Option<Connection> {
        if hint.endpoint == networking.endpoint.id() {
            return None;
        }
        networking
            .endpoint
            .connect(hint.endpoint, rostra_p2p_api::ROSTRA_P2P_V0_ALPN)
            .await
            .ok()
            .map(Connection::from)
    }

    /// Try to fetch event content from multiple peers with some parallelism.
    ///
    /// Reuse the shared store first, then reserve each racing read
    /// independently. Returns true when no further acquisition is needed or
    /// ingestion succeeded. Capacity refusal is a typed temporary DB error,
    /// never a peer failure.
    pub async fn fetch_event_content_from_peers(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        event: VerifiedEvent,
        db: &rostra_client_db::Database,
    ) -> rostra_client_db::DbResult<bool> {
        self.fetch_event_content_from_peers_detailed(networking, peers, event, db)
            .await
            .map(|outcome| outcome.acquisition_satisfied)
    }

    /// Try peers while preserving whether this invocation stored content.
    pub(crate) async fn fetch_event_content_from_peers_detailed(
        &self,
        networking: &ClientNetworking,
        peers: &[RostraId],
        event: VerifiedEvent,
        db: &rostra_client_db::Database,
    ) -> rostra_client_db::DbResult<ContentFetchOutcome> {
        use rostra_client_db::{DbError, PayloadAcquisitionPreparation};

        let reservation = match db.prepare_payload_acquisition_detailed(&event).await? {
            PayloadAcquisitionPreparation::Disabled => None,
            PayloadAcquisitionPreparation::Reserved(reservation) => Some(reservation),
            PayloadAcquisitionPreparation::Materialized => {
                return Ok(ContentFetchOutcome {
                    acquisition_satisfied: true,
                    materialized: true,
                });
            }
            PayloadAcquisitionPreparation::Satisfied => {
                return Ok(ContentFetchOutcome {
                    acquisition_satisfied: true,
                    materialized: false,
                });
            }
            PayloadAcquisitionPreparation::Deferred(reason) => {
                return Err(DbError::PayloadAdmissionPaused { reason });
            }
        };
        let preferred = self
            .is_connected(event.author())
            .await
            .then_some(event.author());
        let peers = crate::payload_holder_order::rank_payload_holders(
            event.event_id,
            peers.iter().copied(),
            preferred,
        );
        // When only one read fits, every ordinary candidate gets its chance
        // before a hinted peer can retain those buffers. With spare capacity,
        // the hint still races within the same four slots.
        let peers: Vec<_> = peers
            .into_iter()
            .map(FetchPeer::Account)
            .chain([FetchPeer::Holder])
            .collect();
        let result = crate::payload_read_race::race_payload_reads(
            &peers,
            || {
                let reservation = reservation.as_ref();
                let buffer = reservation.map(|r| r.try_acquire_buffer()).transpose()?;
                let conversion = reservation.map(|r| r.try_acquire_buffer()).transpose()?;
                Ok((buffer, conversion))
            },
            |peer, (buffer, conversion)| {
                let cache = self.clone();
                async move {
                    if matches!(peer, FetchPeer::Holder) {
                        let _permit = cache.holders.operations.try_acquire().ok()?;
                        let attempt = cache
                            .holders
                            .attempt(event.author(), event.event_id.to_short())?;
                        let hint = attempt.hint;
                        let payload = tokio::time::timeout(
                            hint.timeout().min(cache.peer_operation_deadline),
                            async {
                                let conn = cache.connect_holder(networking, hint).await?;
                                let (content, (buffer, conversion)) = conn
                                    .get_event_content_with_guard(event, (buffer, conversion))
                                    .await
                                    .ok()
                                    .flatten()?;
                                drop(conversion);
                                Some(crate::acquired_payload::AcquiredPayload { content, buffer })
                            },
                        )
                        .await
                        .ok()
                        .flatten()?;
                        attempt.succeeded();
                        return Some(payload);
                    }
                    let FetchPeer::Account(peer_id) = peer else {
                        unreachable!()
                    };
                    crate::task::outbound_deadline::within(
                        crate::task::outbound_deadline::PEER_OPERATION_DEADLINE,
                        async move {
                            let conn = cache.get_or_connect(networking, peer_id).await.ok()?;
                            match conn
                                .get_event_content_with_guard(event, (buffer, conversion))
                                .await
                            {
                                Ok(Some((content, (buffer, conversion)))) => {
                                    drop(conversion);
                                    Some(crate::acquired_payload::AcquiredPayload {
                                        content,
                                        buffer,
                                    })
                                }
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
                        },
                    )
                    .await
                    .ok()
                    .flatten()
                }
            },
        )
        .await
        .map_err(|reason| DbError::PayloadAdmissionPaused { reason })?;

        if let Some(payload) = result {
            let outcome = payload.ingest_with_outcome(db).await?;
            return Ok(ContentFetchOutcome::from_ingest(outcome));
        }
        {
            debug!(
                target: LOG_TARGET,
                event_id = %event.event_id.to_short(),
                "Event content not found from any peer"
            );
        }

        Ok(ContentFetchOutcome {
            acquisition_satisfied: false,
            materialized: false,
        })
    }
}

#[cfg(test)]
mod tests;
