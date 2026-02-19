use std::collections::HashMap;
use std::sync::Arc;

use iroh::Endpoint;
use iroh::endpoint::Incoming;
use rostra_client_db::{DbError, IdsFolloweesRecord, IdsFollowersRecord};
use rostra_core::event::{EventContentRaw, EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, ToShort as _};
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
use tokio::sync::{Semaphore, watch};
use tracing::{debug, info, instrument, trace};

use crate::client::Client;
use crate::{ClientHandle, ClientRefError, ClientRefSnafu};

const LOG_TARGET: &str = "rostra::req_handler";

/// Maximum number of concurrent RPC handlers per connection.
const MAX_CONCURRENT_RPCS_PER_CONNECTION: usize = 32;

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
    self_followees_rx: watch::Receiver<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
    self_followers_rx: watch::Receiver<Arc<HashMap<RostraId, IdsFollowersRecord>>>,
}

impl RequestHandler {
    pub fn new(client: &Client, endpoint: Endpoint) -> Arc<Self> {
        info!(id = %client.rostra_id(), iroh_endpoint = %endpoint.id(), "Starting request handler task");
        Self {
            client: client.handle(),
            endpoint,
            our_id: client.rostra_id(),
            self_followees_rx: client.self_followees_subscribe(),
            self_followers_rx: client.self_followers_subscribe(),
        }
        .into()
    }

    /// Run the thread
    #[instrument(name = "request-handler", skip(self), fields(self_id = %self.our_id.to_short()), ret)]
    pub async fn run(self: Arc<Self>) {
        loop {
            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            };
            let Some(incoming) = self.endpoint.accept().await else {
                debug!(target: LOG_TARGET, "Can't accept any more connection, quitting");
                return;
            };

            trace!(target: LOG_TARGET, "New connection" );
            tokio::spawn(self.clone().handle_incoming(incoming));
        }
    }
    pub async fn handle_incoming(self: Arc<Self>, incoming: Incoming) {
        let peer_addr = incoming.remote_address();
        if let Err(err) = Arc::clone(&self).handle_incoming_try(incoming).await {
            match err {
                // normal, mostly ignore
                IncomingConnectionError::Connection { source: _, .. } => {
                    trace!(target: LOG_TARGET, err = %err.fmt_compact(), %peer_addr, "Client disconnected");
                }
                _ => {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), %peer_addr, "Error handling incoming connection");
                }
            }
        }
    }
    pub async fn handle_incoming_try(
        self: &Arc<Self>,
        incoming: Incoming,
    ) -> IncomingConnectionResult<()> {
        let conn = incoming
            .accept()
            .context(ConnectionStreamSnafu)?
            .await
            .context(ConnectionSnafu)?;

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_RPCS_PER_CONNECTION));

        loop {
            let (send, mut recv) = conn.accept_bi().await.context(ConnectionStreamSnafu)?;
            let (rpc_id, req_msg) = Connection::read_request_raw(&mut recv)
                .await
                .context(RpcSnafu)?;

            debug!(
                target: LOG_TARGET,
                rpc_id = %rpc_id,
                from = %conn.remote_id().to_short(),
                "Rpc request"
            );

            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore never closed");

            // Spawn each RPC handler as a separate task so that blocking
            // RPCs (WAIT_HEAD_UPDATE, WAIT_FOLLOWERS_NEW_HEADS) don't
            // prevent other RPCs on the same connection from being accepted.
            let handler = self.clone();
            tokio::spawn(async move {
                let result = match rpc_id {
                    RpcId::PING => handler.handle_ping_request(req_msg, send).await,
                    RpcId::FEED_EVENT => handler.handle_feed_event(req_msg, send, recv).await,
                    RpcId::GET_EVENT => handler.handle_get_event(req_msg, send, recv).await,
                    RpcId::GET_EVENT_CONTENT => {
                        handler.handle_get_event_content(req_msg, send, recv).await
                    }
                    RpcId::WAIT_HEAD_UPDATE => {
                        handler.handle_wait_head_update(req_msg, send, recv).await
                    }
                    RpcId::GET_HEAD => handler.handle_get_head(req_msg, send, recv).await,
                    RpcId::WAIT_FOLLOWERS_NEW_HEADS => {
                        handler
                            .handle_wait_followers_new_heads(req_msg, send, recv)
                            .await
                    }
                    _ => {
                        debug!(target: LOG_TARGET, %rpc_id, "Unknown RPC ID");
                        return;
                    }
                };
                drop(permit);
                if let Err(err) = result {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), "RPC handler error");
                }
            });
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

        if event.author() == our_id
            || self
                .self_followees_rx
                .borrow()
                .contains_key(&event.author())
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
                client
                    .store_event_too_large(event.event_id, *event.event())
                    .await?;
                Connection::write_return_code(
                    &mut send,
                    FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                )
                .await
                .context(RpcSnafu)?;
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

            client
                .store_event_with_content(event.event_id, &verified_content)
                .await;
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

            // Respond when our head differs from what the client knows.
            // Also wait if we have no heads yet.
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
                heads
                    .into_iter()
                    .next()
                    .expect("Must have at least one element"),
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

        Connection::write_message(&mut send, &GetHeadResponse(heads.into_iter().next()))
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
            let (author, _head) = match new_heads_rx.recv().await {
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
                let followers = self.self_followers_rx.borrow();
                followers.contains_key(&author)
            };

            if !is_relevant {
                continue;
            }

            // Get the full event from the database
            let db = self.client.db()?;
            let heads = db.get_heads(author).await;
            let Some(head) = heads.into_iter().next() else {
                continue;
            };

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
