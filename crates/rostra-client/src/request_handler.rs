use std::collections::HashMap;
use std::sync::Arc;

use iroh::endpoint::Incoming;
use iroh::Endpoint;
use rostra_client_db::{DbError, IdsFolloweesRecord};
use rostra_core::event::{EventContent, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraId;
use rostra_p2p::connection::{
    Connection, FeedEventRequest, FeedEventResponse, GetEventContentRequest,
    GetEventContentResponse, GetEventRequest, GetEventResponse, PingRequest, PingResponse, RpcId,
    RpcMessage as _, WaitHeadUpdateRequest, WaitHeadUpdateResponse, MAX_REQUEST_SIZE,
};
use rostra_p2p::RpcError;
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu};
use tokio::sync::watch;
use tracing::{debug, info, instrument};

use crate::client::Client;
use crate::{ClientHandle, ClientRefError, ClientStorageError};

const LOG_TARGET: &str = "rostra::req_handler";

#[derive(Debug, Snafu)]
pub enum IncomingConnectionError {
    Connection {
        source: iroh::endpoint::ConnectionError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
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
        #[snafu(implicit)]
        location: Location,
    },
    // TODO: more details
    InvalidRequest {
        source: BoxedError,
        #[snafu(implicit)]
        location: Location,
    },
    InvalidSignature {
        source: ed25519_dalek::SignatureError,
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
    ClientStorage {
        source: ClientStorageError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    ClientRefError {
        source: ClientRefError,
        #[snafu(implicit)]
        location: Location,
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
        info!(pkarr_id = %client.rostra_id().try_fmt(), "Starting request handler task");
        Self {
            client: client.handle(),
            endpoint,
            our_id: client.rostra_id(),
            self_followees_rx: client
                .self_followees_subscribe()
                .expect("Can't start folowee checker without storage"),
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

            tokio::spawn(self.clone().handle_incoming(incoming));
        }
    }
    pub async fn handle_incoming(self: Arc<Self>, incoming: Incoming) {
        let peer_addr = incoming.remote_address();
        if let Err(err) = self.handle_incoming_try(incoming).await {
            match err {
                // normal, mostly ignore
                IncomingConnectionError::Connection { source: _, .. } => {
                    debug!(target: LOG_TARGET, err=%err.fmt_compact(), %peer_addr, "Error handling incoming connection");
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
            let (id, req_msg) = Connection::read_request_raw(&mut recv).await?;

            match id {
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
                _ => return UnknownRpcIdSnafu { id }.fail(),
            }
        }
    }

    async fn handle_ping_request(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
    ) -> Result<(), IncomingConnectionError> {
        let req = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;
        Connection::write_success_return_code(&mut send).await?;
        Connection::write_message(&mut send, &PingResponse(req.0)).await?;
        Ok(())
    }

    async fn handle_feed_event(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh::endpoint::SendStream,
        mut read: iroh::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let FeedEventRequest(rostra_core::event::SignedEvent { event, sig }) =
            FeedEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;

        let verified_event = VerifiedEvent::verify_received_as_is(event, sig)
            .boxed()
            .context(InvalidRequestSnafu)?;
        let our_id = self.our_id;

        if event.author == our_id || self.self_followees_rx.borrow().contains_key(&event.author) {
            // accept
        } else {
            Connection::write_return_code(&mut send, FeedEventResponse::RETURN_CODE_DOES_NOT_NEED)
                .await?;
            return Err("Author not needed".into()).context(InvalidRequestSnafu);
        }
        let event_id = event.compute_id();
        event
            .verified_signed_by(sig, event.author)
            .context(InvalidSignatureSnafu)?;

        {
            let client = self.client.app_ref_opt().context(ExitingSnafu)?;

            if client.event_size_limit() < u32::from(event.content_len) {
                client.store_event_too_large(event_id, event).await?;
                Connection::write_return_code(
                    &mut send,
                    FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                )
                .await?;
            }

            if client.does_have_event(event_id).await {
                Connection::write_return_code(
                    &mut send,
                    FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                )
                .await?;
                return Ok(());
            }
        }
        Connection::write_success_return_code(&mut send).await?;
        Connection::write_message(&mut send, &FeedEventResponse).await?;

        let event_content = EventContent::from(
            Connection::read_bao_content(&mut read, event.content_len.into(), event.content_hash)
                .await?,
        );

        {
            let client = self.client.app_ref_opt().context(ExitingSnafu)?;
            let verified_content = VerifiedEventContent::verify(verified_event, event_content)
                .boxed()
                .context(InvalidRequestSnafu)?;

            client
                .store_event_with_content(event_id, &verified_content)
                .await?;
        }

        Connection::write_success_return_code(&mut send).await?;

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
        let storage = client.storage()?;

        let event = storage.get_event(event_id).await;

        Connection::write_success_return_code(&mut send).await?;

        Connection::write_message(&mut send, &GetEventResponse(event.map(|e| e.signed))).await?;

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
        let storage = client.storage()?;

        let content = storage.get_event_content(event_id).await;

        Connection::write_success_return_code(&mut send).await?;

        Connection::write_message(&mut send, &GetEventContentResponse(content.is_some())).await?;

        if let Some(content) = content {
            let event = storage
                .get_event(event_id)
                .await
                .expect("Must have event if we have content");
            Connection::write_bao_content(
                &mut send,
                content.as_ref(),
                event.signed.event.content_hash,
            )
            .await?;
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

        Connection::write_success_return_code(&mut send).await?;

        // Note: do not keep storage around
        let mut head_updated = self.client.storage()??.self_head_subscribe();

        let mut heads;
        loop {
            heads = self.client.storage()??.get_heads_self().await?;

            if heads.contains(&event_id) {
                break;
            }
            head_updated
                .changed()
                .await
                .map_err(|_| ClientStorageError)?;
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
        .await?;
        Ok(())
    }
}
