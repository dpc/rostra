use std::collections::HashSet;

use futures::stream::{FuturesUnordered, StreamExt as _};
use iroh_base::EndpointAddr;
use rostra_core::Timestamp;
use rostra_core::event::IrohNodeId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::ConnectionSnafu;
use rostra_p2p::connection::Connection;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::FmtCompact as _;
use rostra_util_fmt::AsFmtOption as _;
use snafu::ResultExt as _;
use tracing::{debug, trace, warn};

use crate::client::{Client, NodeSource};
use crate::error::{
    ConnectError, ConnectIrohSnafu, ConnectResult, NodeInBackoffSnafu, PeerUnavailableSnafu,
    ResolveSnafu,
};
// ConnectIrohSnafu is used for .context() in connect_ticket
use crate::id::CompactTicket;

const LOG_TARGET: &str = "rostra::client-net";

/// Result of attempting to connect to an endpoint.
#[derive(Debug)]
pub enum EndpointConnectResult {
    /// Successfully connected
    Success(Connection),
    /// Connection failed
    Failed(ConnectError),
    /// Node is in backoff, should not attempt
    InBackoff,
    /// Skipped (e.g., trying to connect to self)
    Skipped,
}

impl Client {
    /// Connect to a specific iroh endpoint with backoff handling.
    ///
    /// This is the core connection function that:
    /// - Checks if the node is in backoff before attempting
    /// - Updates p2p_state on success/failure
    /// - Resets backoff on success
    /// - Increases backoff on failure (exponential, capped at 10 minutes)
    ///
    /// Returns `EndpointConnectResult` to indicate the outcome.
    ///
    /// Note: The connection+ping logic here is partially duplicated in
    /// `connect_uncached`'s parallel connection loop. See that function's
    /// comments for why it doesn't use this method.
    pub async fn connect_to_endpoint(
        &self,
        endpoint_addr: EndpointAddr,
        source: NodeSource,
        rostra_id: Option<RostraId>,
    ) -> EndpointConnectResult {
        let node_id = IrohNodeId::from_bytes(*endpoint_addr.id.as_bytes());

        // Skip connecting to our own endpoint
        if endpoint_addr.id == self.endpoint.id() {
            return EndpointConnectResult::Skipped;
        }

        // Check if node is in backoff
        if self.p2p_state.is_node_in_backoff(node_id).await {
            if let Some(remaining) = self.p2p_state.get_node_backoff_remaining(node_id).await {
                trace!(
                    target: LOG_TARGET,
                    %node_id,
                    remaining_secs = remaining.as_secs(),
                    "Node is in backoff, skipping connection attempt"
                );
            }
            return EndpointConnectResult::InBackoff;
        }

        // Track attempt
        let now = Timestamp::now();
        self.p2p_state
            .update_node(node_id, |state| {
                state.last_attempt = Some(now);
                state.source = source;
                state.rostra_id = rostra_id;
            })
            .await;

        debug!(
            target: LOG_TARGET,
            iroh_id = %endpoint_addr.id,
            rostra_id = %rostra_id.map(|id| id.to_short().to_string()).unwrap_or_default(),
            "Connecting to endpoint"
        );

        // Attempt connection
        let conn_result = self
            .endpoint
            .connect(endpoint_addr, ROSTRA_P2P_V0_ALPN)
            .await;

        trace!(
            target: LOG_TARGET,
            %node_id,
            err = %conn_result.as_ref().err().fmt_option(),
            "Iroh connect result"
        );

        match conn_result {
            Ok(conn) => {
                let conn = Connection::from(conn);

                // Verify connection with ping
                let ping_result = conn.ping(0).await;
                trace!(
                    target: LOG_TARGET,
                    %node_id,
                    err = %ping_result.as_ref().err().fmt_option(),
                    "Ping result"
                );

                match ping_result {
                    Ok(_) => {
                        let now = Timestamp::now();
                        self.p2p_state
                            .update_node(node_id, |state| state.record_success(now))
                            .await;
                        if let Some(id) = rostra_id {
                            self.p2p_state
                                .update(id, |state| state.last_success = Some(now))
                                .await;
                        }
                        debug!(
                            target: LOG_TARGET,
                            %node_id,
                            rostra_id = %rostra_id.fmt_option(),
                            "Connected to endpoint successfully"
                        );
                        EndpointConnectResult::Success(conn)
                    }
                    Err(err) => {
                        let now = Timestamp::now();
                        self.p2p_state
                            .update_node(node_id, |state| state.record_failure(now))
                            .await;
                        debug!(
                            target: LOG_TARGET,
                            %node_id,
                            err = %err.fmt_compact(),
                            "Ping failed after connection"
                        );
                        EndpointConnectResult::Failed(ConnectError::PeerUnavailable)
                    }
                }
            }
            Err(err) => {
                let now = Timestamp::now();
                self.p2p_state
                    .update_node(node_id, |state| state.record_failure(now))
                    .await;
                if let Some(id) = rostra_id {
                    self.p2p_state
                        .update(id, |state| state.last_failure = Some(now))
                        .await;
                }
                debug!(
                    target: LOG_TARGET,
                    %node_id,
                    err = %err.fmt_compact(),
                    "Failed to connect to endpoint"
                );
                EndpointConnectResult::Failed(ConnectError::ConnectIroh { source: err })
            }
        }
    }

    /// Connect to a RostraId by trying all known endpoints in parallel.
    ///
    /// Note: The parallel loop below duplicates some logic from
    /// `connect_to_endpoint` rather than calling it. This is intentional
    /// because:
    /// 1. The spawned futures cannot borrow `&self` (Rust async limitation)
    /// 2. We want to check backoff BEFORE spawning futures (not inside them)
    /// 3. We want to update state AFTER futures complete (in the main loop, not
    ///    racing inside futures)
    /// 4. The connection+ping code is simple (~10 lines) and clearer when
    ///    inline
    pub async fn connect_uncached(&self, id: RostraId) -> ConnectResult<Connection> {
        let now = Timestamp::now();
        self.p2p_state
            .update(id, |state| state.last_attempt = Some(now))
            .await;

        let endpoints = self.db.get_id_endpoints(id).await;

        debug!(
            target: LOG_TARGET,
            %id,
            num_endpoints = endpoints.len(),
            "Connecting to peer, trying known endpoints"
        );

        // Try all known endpoints in parallel
        let mut connection_futures = FuturesUnordered::new();

        let node_ids: HashSet<_> = endpoints
            .into_keys()
            .map(|(_ts, node_id)| node_id)
            .collect();

        for node_id in node_ids.clone() {
            let Ok(pub_key) = iroh::PublicKey::from_bytes(&node_id.to_bytes()) else {
                debug!(target: LOG_TARGET, %id, "Invalid iroh id for rostra id found");
                continue;
            };

            // Check backoff before spawning the future
            if self.p2p_state.is_node_in_backoff(node_id).await {
                trace!(
                    target: LOG_TARGET,
                    %node_id,
                    %id,
                    "Node is in backoff, skipping"
                );
                continue;
            }

            // Track attempt per node
            let now = Timestamp::now();
            self.p2p_state
                .update_node(node_id, |state| {
                    state.last_attempt = Some(now);
                    state.source = NodeSource::NodeAnnouncement;
                    state.rostra_id = Some(id);
                })
                .await;

            let endpoint = self.endpoint.clone();
            let our_id = self.endpoint.id();
            connection_futures.push(async move {
                if pub_key == our_id {
                    // Skip connecting to our own Id
                    return (node_id, Err(ConnectError::PeerUnavailable));
                }

                let result = async {
                    let conn_result = endpoint
                        .connect(pub_key, ROSTRA_P2P_V0_ALPN)
                        .await
                        .context(ConnectionSnafu);
                    trace!(target: LOG_TARGET, %node_id, err = %conn_result.as_ref().err().fmt_option(), "Iroh connect result");
                    let conn = Connection::from(conn_result?);

                    // Verify connection with ping
                    let ping_result = conn.ping(0).await;
                    trace!(target: LOG_TARGET, %node_id, err = %ping_result.as_ref().err().fmt_option(), "Ping result");
                    ping_result?;
                    Ok::<_, rostra_p2p::RpcError>(conn)
                }
                .await;
                (
                    node_id,
                    result.map_err(|_| ConnectError::PeerUnavailable),
                )
            });
        }

        // Try all connections in parallel, take first success
        while let Some((node_id, result)) = connection_futures.next().await {
            debug!(target: LOG_TARGET, %node_id, err = %result.as_ref().err().fmt_option(), "Connection result");

            match result {
                Ok(conn) => {
                    let now = Timestamp::now();
                    self.p2p_state
                        .update(id, |state| state.last_success = Some(now))
                        .await;
                    self.p2p_state
                        .update_node(node_id, |state| state.record_success(now))
                        .await;
                    debug!(
                        target: LOG_TARGET,
                        %id,
                        %node_id,
                        "Successfully connected to peer via known endpoint"
                    );
                    return Ok(conn);
                }
                Err(_err) => {
                    let now = Timestamp::now();
                    self.p2p_state
                        .update_node(node_id, |state| state.record_failure(now))
                        .await;
                }
            }
        }

        if !node_ids.is_empty() {
            debug!(
                target: LOG_TARGET,
                %id,
                "All known endpoints failed, trying pkarr resolution"
            );
        }

        // Fall back to pkarr if no known endpoints worked
        self.connect_by_pkarr_resolution(id).await
    }

    pub async fn connect_by_pkarr_resolution(&self, id: RostraId) -> ConnectResult<Connection> {
        let ticket = self.resolve_id_ticket(id).await.context(ResolveSnafu)?;

        let endpoint_addr = EndpointAddr::from(ticket);
        let node_id = IrohNodeId::from_bytes(*endpoint_addr.id.as_bytes());

        match self
            .connect_to_endpoint(endpoint_addr, NodeSource::Pkarr, Some(id))
            .await
        {
            EndpointConnectResult::Success(conn) => Ok(conn),
            EndpointConnectResult::Failed(err) => Err(err),
            EndpointConnectResult::InBackoff => {
                if let Some(remaining) = self.p2p_state.get_node_backoff_remaining(node_id).await {
                    warn!(
                        target: LOG_TARGET,
                        %id,
                        %node_id,
                        remaining_secs = remaining.as_secs(),
                        "Cannot connect to peer - node is in backoff"
                    );
                }
                Err(NodeInBackoffSnafu.build())
            }
            EndpointConnectResult::Skipped => Err(PeerUnavailableSnafu.build()),
        }
    }

    pub async fn connect_ticket(&self, ticket: CompactTicket) -> ConnectResult<Connection> {
        // Note: connect_ticket doesn't use backoff since tickets are typically
        // provided by users and should be attempted regardless of previous failures
        Ok(self
            .endpoint
            .connect(ticket, ROSTRA_P2P_V0_ALPN)
            .await
            .context(ConnectIrohSnafu)?
            .into())
    }
}
