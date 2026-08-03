use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use iroh::Endpoint;
use iroh::endpoint::Incoming;
use n0_future::task::AbortOnDropHandle;
use rostra_client_db::{CurrentState, DbError, IdsFolloweesRecord, IdsFollowersRecord};
use rostra_core::event::{EventContentRaw, EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraId;
use rostra_p2p::RpcError;
use rostra_p2p::connection::{
    Connection, FeedEventRequest, FeedEventResponse, GetEventContentRequest,
    GetEventContentResponse, GetEventRequest, GetEventResponse, GetHeadRequest, GetHeadResponse,
    MAX_REQUEST_SIZE, PingRequest, PingResponse, RpcId, RpcMessage as _,
    WaitFollowersNewHeadsRequest, WaitFollowersNewHeadsResponse, WaitHeadUpdateRequest,
    WaitHeadUpdateResponse,
};
use rostra_p2p::util::ToShort as _;
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tracing::{debug, error, info, instrument, trace};

use crate::client::{Client, ClientRefSnafu};
use crate::error::StoreEventError;
use crate::task::head_selection::sample_head;
use crate::{ClientHandle, ClientRefError};

const LOG_TARGET: &str = "rostra::req_handler";

/// Maximum number of concurrent RPC handlers per connection.
const MAX_CONCURRENT_RPCS_PER_CONNECTION: usize = 32;

/// Maximum number of inbound connections owned by one client.
const MAX_INBOUND_CONNECTIONS: usize = 128;

/// Maximum number of inbound RPC handlers owned by one client.
const MAX_CONCURRENT_INBOUND_RPCS: usize = 256;

/// Slots reserved so long polls cannot starve finite RPCs.
const RESERVED_ORDINARY_INBOUND_RPCS: usize = 64;

/// Maximum time an accepted transport may spend completing its handshake.
const INBOUND_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum time an idle connection may remain without an active RPC.
const INBOUND_IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// Maximum time a peer may spend sending one bounded request header.
const INBOUND_REQUEST_HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum complete lifetime of an ordinary finite RPC.
#[cfg(not(test))]
const ORDINARY_RPC_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(test)]
const ORDINARY_RPC_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
struct InboundAdmission {
    connections: Arc<Semaphore>,
    shared_rpcs: Arc<Semaphore>,
    reserved_ordinary_rpcs: Arc<Semaphore>,
}

impl InboundAdmission {
    fn new() -> Self {
        Self {
            connections: Arc::new(Semaphore::new(MAX_INBOUND_CONNECTIONS)),
            shared_rpcs: Arc::new(Semaphore::new(
                MAX_CONCURRENT_INBOUND_RPCS - RESERVED_ORDINARY_INBOUND_RPCS,
            )),
            reserved_ordinary_rpcs: Arc::new(Semaphore::new(RESERVED_ORDINARY_INBOUND_RPCS)),
        }
    }

    fn try_admit_connection(&self) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        self.connections.clone().try_acquire_owned()
    }

    fn try_admit_rpc(&self, long_poll: bool) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        if long_poll {
            return self.shared_rpcs.clone().try_acquire_owned();
        }
        self.reserved_ordinary_rpcs
            .clone()
            .try_acquire_owned()
            .or_else(|_| self.shared_rpcs.clone().try_acquire_owned())
    }
}

#[derive(Debug, Snafu)]
pub enum IncomingConnectionError {
    Connection {
        source: iroh::endpoint::ConnectingError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Connection stream error: {source}"))]
    ConnectionStream {
        source: iroh::endpoint::ConnectionError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Connection handshake timed out"))]
    HandshakeTimeout {
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Idle connection timed out"))]
    IdleTimeout {
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Request header timed out"))]
    RequestHeaderTimeout {
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Ordinary RPC timed out"))]
    OrdinaryRpcTimeout {
        #[snafu(implicit)]
        location: Location,
    },
    Rpc {
        source: RpcError,
        #[snafu(implicit)]
        location: Location,
    },
    Decoding {
        source: bincode::error::DecodeError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    Db {
        source: DbError,
    },
    #[snafu(transparent)]
    StoreEvent {
        source: StoreEventError,
    },
    // TODO: more details
    InvalidRequest {
        source: BoxedError,
        #[snafu(implicit)]
        location: Location,
    },
    Exiting,
    #[snafu(display("Unknown RPC ID: {id}"))]
    UnknownRpcId {
        id: RpcId,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    ClientRefError {
        source: ClientRefError,
    },
}
pub type IncomingConnectionResult<T> = std::result::Result<T, IncomingConnectionError>;

pub struct RequestHandler {
    client: ClientHandle,
    endpoint: Endpoint,
    our_id: RostraId,
    self_followees: CurrentState<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
    self_followers: CurrentState<Arc<HashMap<RostraId, IdsFollowersRecord>>>,
    inbound_admission: InboundAdmission,
}

impl RequestHandler {
    pub fn new(client: &Client, endpoint: Endpoint) -> Arc<Self> {
        info!(id = %client.rostra_id().fmt_short(), iroh_endpoint = %endpoint.id(), "Starting request handler task");
        Self {
            client: client.handle(),
            endpoint,
            our_id: client.rostra_id(),
            self_followees: client.self_followees_subscribe(),
            self_followers: client.self_followers_subscribe(),
            inbound_admission: InboundAdmission::new(),
        }
        .into()
    }

    /// Run the thread
    #[instrument(name = "request-handler", skip(self), fields(self_id = %self.our_id.fmt_short()), ret)]
    pub async fn run(self: Arc<Self>) {
        let mut connection_tasks = FuturesUnordered::new();
        loop {
            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            };

            tokio::select! {
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        debug!(target: LOG_TARGET, "Can't accept any more connection, quitting");
                        return;
                    };

                    let Ok(connection_permit) = self.inbound_admission.try_admit_connection() else {
                        debug!(
                            target: LOG_TARGET,
                            max_connections = MAX_INBOUND_CONNECTIONS,
                            "Rejecting connection: client-wide inbound connection limit reached"
                        );
                        continue;
                    };
                    trace!(target: LOG_TARGET, "New connection" );
                    connection_tasks.push(AbortOnDropHandle::new(tokio::spawn(
                        self.clone().handle_incoming(incoming, connection_permit),
                    )));
                }
                Some(_) = connection_tasks.next(), if !connection_tasks.is_empty() => {}
            }
        }
    }
    async fn handle_incoming(
        self: Arc<Self>,
        incoming: Incoming,
        _connection_permit: OwnedSemaphorePermit,
    ) {
        let peer_addr = incoming.remote_addr();
        if let Err(err) = Arc::clone(&self).handle_incoming_try(incoming).await {
            match err {
                // normal, mostly ignore
                IncomingConnectionError::Connection { .. } => {
                    trace!(target: LOG_TARGET, err = %err.fmt_compact(), ?peer_addr, "Client disconnected");
                }
                _ => {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), ?peer_addr, "Error handling incoming connection");
                }
            }
        }
    }
    async fn handle_incoming_try(
        self: &Arc<Self>,
        incoming: Incoming,
    ) -> IncomingConnectionResult<()> {
        let connecting = incoming.accept().context(ConnectionStreamSnafu)?;
        let conn = tokio::time::timeout(INBOUND_HANDSHAKE_TIMEOUT, connecting)
            .await
            .map_err(|_| HandshakeTimeoutSnafu.build())?
            .context(ConnectionSnafu)?;
        conn.set_max_concurrent_bi_streams(
            u32::try_from(MAX_CONCURRENT_RPCS_PER_CONNECTION)
                .expect("RPC limit fits u32")
                .into(),
        );
        conn.set_max_concurrent_uni_streams(0u32.into());

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_RPCS_PER_CONNECTION));
        let mut rpc_tasks = FuturesUnordered::new();

        loop {
            tokio::select! {
                _ = tokio::time::sleep(INBOUND_IDLE_TIMEOUT), if rpc_tasks.is_empty() => {
                    return IdleTimeoutSnafu.fail();
                }
                stream = conn.accept_bi() => {
                    let (send, mut recv) = stream.context(ConnectionStreamSnafu)?;
                    let (rpc_id, req_msg) = tokio::time::timeout(
                        INBOUND_REQUEST_HEADER_TIMEOUT,
                        Connection::read_request_raw(&mut recv),
                    )
                        .await
                        .map_err(|_| RequestHeaderTimeoutSnafu.build())?
                        .context(RpcSnafu)?;

                    debug!(
                        target: LOG_TARGET,
                        rpc_id = %rpc_id,
                        from = %conn.remote_id().to_short(),
                        "Rpc request"
                    );

                    let Ok(connection_permit) = semaphore.clone().try_acquire_owned() else {
                        debug!(
                            target: LOG_TARGET,
                            rpc_id = %rpc_id,
                            max_rpcs = MAX_CONCURRENT_RPCS_PER_CONNECTION,
                            "Rejecting RPC: per-connection limit reached"
                        );
                        continue;
                    };
                    let long_poll = matches!(
                        rpc_id,
                        RpcId::WAIT_HEAD_UPDATE | RpcId::WAIT_FOLLOWERS_NEW_HEADS
                    );
                    let Ok(client_permit) = self.inbound_admission.try_admit_rpc(long_poll) else {
                        debug!(
                            target: LOG_TARGET,
                            rpc_id = %rpc_id,
                            max_rpcs = MAX_CONCURRENT_INBOUND_RPCS,
                            "Rejecting RPC: client-wide inbound RPC limit reached"
                        );
                        continue;
                    };

                    // Spawn each RPC handler as a separate task so that blocking
                    // RPCs (WAIT_HEAD_UPDATE, WAIT_FOLLOWERS_NEW_HEADS) don't
                    // prevent other RPCs on the same connection from being accepted.
                    let handler = self.clone();
                    rpc_tasks.push(AbortOnDropHandle::new(tokio::spawn(async move {
                        let request = async {
                            match rpc_id {
                                RpcId::PING => handler.handle_ping_request(req_msg, send).await,
                                RpcId::FEED_EVENT => {
                                    handler.handle_feed_event(req_msg, send, recv).await
                                }
                                RpcId::GET_EVENT => {
                                    handler.handle_get_event(req_msg, send, recv).await
                                }
                                RpcId::GET_EVENT_CONTENT => {
                                    handler.handle_get_event_content(req_msg, send, recv).await
                                }
                                RpcId::WAIT_HEAD_UPDATE => {
                                    handler.handle_wait_head_update(req_msg, send, recv).await
                                }
                                RpcId::GET_HEAD => {
                                    handler.handle_get_head(req_msg, send, recv).await
                                }
                                RpcId::WAIT_FOLLOWERS_NEW_HEADS => {
                                    handler
                                        .handle_wait_followers_new_heads(req_msg, send, recv)
                                        .await
                                }
                                _ => {
                                    debug!(target: LOG_TARGET, %rpc_id, "Unknown RPC ID");
                                    Ok(())
                                }
                            }
                        };
                        let result = if long_poll {
                            request.await
                        } else {
                            match tokio::time::timeout(ORDINARY_RPC_TIMEOUT, request).await {
                                Ok(result) => result,
                                Err(_) => Err(OrdinaryRpcTimeoutSnafu.build()),
                            }
                        };
                        drop((connection_permit, client_permit));
                        if let Err(err) = result {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "RPC handler error");
                        }
                    })));
                }
                Some(_) = rpc_tasks.next(), if !rpc_tasks.is_empty() => {}
            }
        }
    }

    async fn handle_ping_request(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
    ) -> Result<(), IncomingConnectionError> {
        let req = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;
        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;
        Connection::write_message(&mut send, &PingResponse(req.0))
            .await
            .context(RpcSnafu)?;
        Ok(())
    }

    async fn handle_feed_event(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        mut read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let FeedEventRequest(event) =
            FeedEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;
        let our_id = self.our_id;

        if event.author() == our_id || self.self_followees.snapshot().contains_key(&event.author())
        {
            // accept
        } else {
            Connection::write_return_code(&mut send, FeedEventResponse::RETURN_CODE_DOES_NOT_NEED)
                .await
                .context(RpcSnafu)?;
            return Err("Author not needed".into()).context(InvalidRequestSnafu);
        }

        let event = VerifiedEvent::verify_received_as_is(event)
            .boxed()
            .context(InvalidRequestSnafu)?;
        {
            let client = self.client.app_ref_opt().context(ExitingSnafu)?;

            if client.event_size_limit() < event.content_len() {
                client.store_event_too_large(&event).await?;
                Connection::write_return_code(
                    &mut send,
                    FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                )
                .await
                .context(RpcSnafu)?;
                return Ok(());
            }

            if client.does_have_event(event.event_id).await {
                Connection::write_return_code(
                    &mut send,
                    FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                )
                .await
                .context(RpcSnafu)?;
                return Ok(());
            }
        }
        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;
        Connection::write_message(&mut send, &FeedEventResponse)
            .await
            .context(RpcSnafu)?;

        let event_content = EventContentRaw::from(
            Connection::read_bao_content(&mut read, event.content_len(), event.content_hash())
                .await
                .context(RpcSnafu)?,
        );

        {
            let client = self.client.app_ref_opt().context(ExitingSnafu)?;
            let verified_content = VerifiedEventContent::verify(event, event_content)
                .boxed()
                .context(InvalidRequestSnafu)?;

            if let Err(err) = client
                .store_event_with_content(event.event_id, &verified_content)
                .await
            {
                error!(
                    target: LOG_TARGET,
                    author_id = %verified_content.author(),
                    event_id = %verified_content.event_id(),
                    err = %err,
                    "Failed to store event received through FEED_EVENT"
                );
                return Err(err.into());
            }
        }

        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;

        Ok(())
    }

    async fn handle_get_event(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        _read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let GetEventRequest(event_id) =
            GetEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;

        let client = self.client.client_ref()?;
        let storage = client.db();

        let event = storage.get_event(event_id).await;

        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;

        Connection::write_message(&mut send, &GetEventResponse(event.map(|e| e.signed)))
            .await
            .context(RpcSnafu)?;

        Ok(())
    }

    async fn handle_get_event_content(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        _read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let GetEventContentRequest(event_id) =
            GetEventContentRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg)
                .context(DecodingSnafu)?;

        let client = self.client.client_ref()?;
        let db = client.db();

        let content = db.get_event_content(event_id).await;

        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;

        Connection::write_message(&mut send, &GetEventContentResponse(content.is_some()))
            .await
            .context(RpcSnafu)?;

        if let Some(content) = content {
            let event = db
                .get_event(event_id)
                .await
                .expect("Must have event if we have content");
            Connection::write_bao_content(&mut send, content.as_ref(), event.content_hash())
                .await
                .context(RpcSnafu)?;
        }

        Ok(())
    }

    async fn handle_wait_head_update(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        _read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let WaitHeadUpdateRequest(event_id) =
            WaitHeadUpdateRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg)
                .context(DecodingSnafu)?;

        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;

        // Note: do not keep storage around
        let mut head_updated = self.client.db()?.self_head_subscribe();

        let mut heads;
        loop {
            heads = self.client.db()?.get_heads_self().await;

            // This single-head cursor only detects replacement/removal of the
            // known head. An existing sibling does not satisfy the predicate
            // while `event_id` remains in the current head set.
            if !heads.is_empty() && !heads.contains(&event_id) {
                break;
            }
            head_updated
                .changed()
                .await
                .map_err(|_| ClientRefSnafu.build())?;
        }

        Connection::write_message(
            &mut send,
            &WaitHeadUpdateResponse(
                sample_head(&heads).expect("loop exits only for a nonempty head set"),
            ),
        )
        .await
        .context(RpcSnafu)?;
        Ok(())
    }

    async fn handle_get_head(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        _read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let GetHeadRequest(id) =
            GetHeadRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;

        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;

        let heads = self.client.db()?.get_heads(id).await;

        Connection::write_message(&mut send, &GetHeadResponse(sample_head(&heads)))
            .await
            .context(RpcSnafu)?;
        Ok(())
    }

    async fn handle_wait_followers_new_heads(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        _read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let WaitFollowersNewHeadsRequest =
            WaitFollowersNewHeadsRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg)
                .context(DecodingSnafu)?;

        Connection::write_success_return_code(&mut send)
            .await
            .context(RpcSnafu)?;

        // Subscribe to new heads broadcast
        let mut new_heads_rx = self.client.db()?.new_heads_subscribe();

        loop {
            let (author, head) = match new_heads_rx.recv().await {
                Ok(msg) => msg,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ClientRefSnafu.build().into());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Missed some updates, continue waiting
                    continue;
                }
            };

            // Check if author is a direct follower (not extended).
            // Own head changes are served via WAIT_HEAD_UPDATE instead.
            let is_relevant = {
                let followers = self.self_followers.snapshot();
                followers.contains_key(&author)
            };

            if !is_relevant {
                continue;
            }

            // Return the exact head that caused this notification. Selecting
            // another current tip here can hide the newly discovered branch.
            let db = self.client.db()?;
            let Some(event) = db.get_event(head).await else {
                continue;
            };

            Connection::write_message(
                &mut send,
                &WaitFollowersNewHeadsResponse {
                    author,
                    event: event.signed,
                },
            )
            .await
            .context(RpcSnafu)?;
            return Ok(());
        }
    }
}

#[cfg(test)]
#[path = "request_handler/tests.rs"]
mod tests;
