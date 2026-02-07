use std::collections::HashSet;

use futures::stream::{FuturesUnordered, StreamExt as _};
use iroh_base::EndpointAddr;
use rostra_core::Timestamp;
use rostra_core::event::IrohNodeId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::connection::Connection;
use rostra_p2p::{ConnectionSnafu, RpcError};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::FmtCompact as _;
use rostra_util_fmt::AsFmtOption as _;
use snafu::ResultExt as _;
use tracing::{debug, trace};

use crate::client::{Client, NodeSource};
use crate::error::{ConnectIrohSnafu, ConnectResult, PeerUnavailableSnafu, ResolveSnafu};
use crate::id::CompactTicket;

const LOG_TARGET: &str = "rostra::client-net";

impl Client {
    pub async fn connect_uncached(&self, id: RostraId) -> ConnectResult<Connection> {
        let now = Timestamp::now();
        self.p2p_state
            .update(id, |state| state.last_attempt = Some(now))
            .await;

        let endpoints = self.db.get_id_endpoints(id).await;

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

            if pub_key == self.endpoint.id() {
                // Skip connecting to our own Id
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
            connection_futures.push(async move {
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
                    Ok::<_, RpcError>(conn)
                }
                .await;
                (node_id, result)
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
                        .update_node(node_id, |state| state.last_success = Some(now))
                        .await;
                    return Ok(conn);
                }
                Err(err) => {
                    let now = Timestamp::now();
                    self.p2p_state
                        .update_node(node_id, |state| state.last_failure = Some(now))
                        .await;
                    debug!(
                        target: LOG_TARGET,
                        %id,
                        %node_id,
                        err = %err.fmt_compact(),
                        "Failed to connect to endpoint"
                    );
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
        if endpoint_addr.id == self.endpoint.id() {
            // If we are trying to connect to our own Id, we want to connect (if possible)
            // with some other node.
            return Err(PeerUnavailableSnafu.build());
        }

        let node_id = IrohNodeId::from_bytes(*endpoint_addr.id.as_bytes());

        // Track attempt per node (from pkarr source)
        let now = Timestamp::now();
        self.p2p_state
            .update_node(node_id, |state| {
                state.last_attempt = Some(now);
                state.source = NodeSource::Pkarr;
                state.rostra_id = Some(id);
            })
            .await;

        debug!(target: LOG_TARGET, iroh_id = %endpoint_addr.id, id = %id.to_short(), "Connecting after pkarr resolution");
        match self
            .endpoint
            .connect(endpoint_addr, ROSTRA_P2P_V0_ALPN)
            .await
        {
            Ok(conn) => {
                let now = Timestamp::now();
                self.p2p_state
                    .update(id, |state| state.last_success = Some(now))
                    .await;
                self.p2p_state
                    .update_node(node_id, |state| state.last_success = Some(now))
                    .await;
                Ok(conn.into())
            }
            Err(err) => {
                let now = Timestamp::now();
                self.p2p_state
                    .update(id, |state| state.last_failure = Some(now))
                    .await;
                self.p2p_state
                    .update_node(node_id, |state| state.last_failure = Some(now))
                    .await;
                Err(err).context(ConnectIrohSnafu)
            }
        }
    }

    pub async fn connect_ticket(&self, ticket: CompactTicket) -> ConnectResult<Connection> {
        Ok(self
            .endpoint
            .connect(ticket, ROSTRA_P2P_V0_ALPN)
            .await
            .context(ConnectIrohSnafu)?
            .into())
    }
}
