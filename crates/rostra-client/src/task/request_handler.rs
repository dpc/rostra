use std::collections::HashMap;
use std::sync::Arc;

use iroh::Endpoint;
use iroh::endpoint::Incoming;
use rostra_client_db::{DbError, IdsFolloweesRecord};
use rostra_core::event::{EventContent, EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraId;
use rostra_p2p::RpcError;
use rostra_p2p::connection::{
    Connection, FeedEventRequest, FeedEventResponse, GetEventContentRequest,
    GetEventContentResponse, GetEventRequest, GetEventResponse, GetHeadRequest, GetHeadResponse,
    MAX_REQUEST_SIZE, PingRequest, PingResponse, RpcId, RpcMessage as _, WaitHeadUpdateRequest,
    WaitHeadUpdateResponse,
};
use rostra_p2p::util::ToShort as _;
use rostra_util_error::{BoxedError, FmtCompact as _};
use rostra_util_fmt::AsFmtOption as _;
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu};
use tokio::sync::watch;
use tracing::{debug, info, instrument, trace};

use crate::client::Client;
use crate::{ClientHandle, ClientRefError, ClientRefSnafu};

const LOG_TARGET: &str = "rostra::req_handler";

#[derive(Debug, Snafu)]
pub enum IncomingConnectionError {
    Connection {
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
    self_followees_rx: watch::Receiver<HashMap<RostraId, IdsFolloweesRecord>>,
}

impl RequestHandler {
    pub fn new(client: &Client, endpoint: Endpoint) -> Arc<Self> {
        info!(id = %client.rostra_id(), iroh_endpoint = %endpoint.node_id(), "Starting request handler task");
        Self {
            client: client.handle(),
            endpoint,
            our_id: client.rostra_id(),
            self_followees_rx: client.self_followees_subscribe(),
        }
        .into()
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
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
        if let Err(err) = self.handle_incoming_try(incoming).await {
            match err {
                // normal, mostly ignore
                IncomingConnectionError::Connection { source: _, .. } => {
                    trace!(target: LOG_TARGET, err=%err.fmt_compact(), %peer_addr, "Client disconnected");
                }
                _ => {
                    debug!(target: LOG_TARGET, err=%err.fmt_compact(), %peer_addr, "Error handling incoming connection");
                }
            }
        }
    }
    pub async fn handle_incoming_try(&self, incoming: Incoming) -> IncomingConnectionResult<()> {
        let conn = incoming
            .accept()
            .context(ConnectionSnafu)?
            .await
            .context(ConnectionSnafu)?;

        loop {
            let (send, mut recv) = conn.accept_bi().await.context(ConnectionSnafu)?;
            let (rpc_id, req_msg) = Connection::read_request_raw(&mut recv)
                .await
                .context(RpcSnafu)?;

            debug!(
                target: LOG_TARGET,
                rpc_id = %rpc_id,
                from = %conn.remote_node_id().ok().map(|id| id.to_short()).fmt_option(),
                "Rpc request"
            );

            match rpc_id {
                RpcId::PING => {
                    self.handle_ping_request(req_msg, send).await?;
                }
                RpcId::FEED_EVENT => {
                    self.handle_feed_event(req_msg, send, recv).await?;
                }
                RpcId::GET_EVENT => {
                    self.handle_get_event(req_msg, send, recv).await?;
                }
                RpcId::GET_EVENT_CONTENT => {
                    self.handle_get_event_content(req_msg, send, recv).await?;
                }
                RpcId::WAIT_HEAD_UPDATE => {
                    self.handle_wait_head_update(req_msg, send, recv).await?;
                }
                RpcId::GET_HEAD => {
                    self.handle_get_head(req_msg, send, recv).await?;
                }
                _ => return UnknownRpcIdSnafu { id: rpc_id }.fail(),
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

        let event_content = EventContent::from(
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

            if heads.contains(&event_id) {
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
}
