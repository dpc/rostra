use std::sync::Arc;

use bao_tree::io::outboard::EmptyOutboard;
use bao_tree::io::round_up_to_chunks;
use bao_tree::{BaoTree, BlockSize, ByteRanges};
use convi::CastInto as _;
use iroh_io::TokioStreamReader;
use iroh_net::endpoint::Incoming;
use iroh_net::Endpoint;
use rostra_core::id::RostraId;
use rostra_p2p::connection::{
    Connection, FeedEventRequest, FeedEventResponse, GetEventRequest, PingRequest, PingResponse,
    RpcId, RpcMessage as _, MAX_REQUEST_SIZE,
};
use rostra_p2p::RpcError;
use rostra_util_error::FmtCompact as _;
use snafu::{OptionExt as _, ResultExt as _, Snafu};
use tracing::{debug, info, instrument};

use crate::client::Client;
use crate::db::DbError;
use crate::{ClientHandle, ClientRefError, ClientStorageError};

const LOG_TARGET: &str = "rostra::client::req_handler";

#[derive(Debug, Snafu)]
pub enum IncomingConnectionError {
    Connection {
        source: iroh_net::endpoint::ConnectionError,
    },
    #[snafu(transparent)]
    Rpc {
        source: RpcError,
    },
    Decoding {
        source: bincode::error::DecodeError,
    },
    DecodingBao {
        source: bao_tree::io::DecodeError,
    },
    #[snafu(transparent)]
    Db {
        source: DbError,
    },
    // TODO: more details
    InvalidRequest,
    InvalidSignature {
        source: ed25519_dalek::SignatureError,
    },
    Exiting,
    #[snafu(display("Unknown RPC ID: {id}"))]
    UnknownRpcId {
        id: RpcId,
    },
    #[snafu(transparent)]
    ClientStorage {
        source: ClientStorageError,
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
}

impl RequestHandler {
    pub fn new(app: &Client, endpoint: Endpoint) -> Arc<Self> {
        info!(pkarr_id = %app.rostra_id().try_fmt(), "Starting request handler task");
        Self {
            client: app.handle(),
            endpoint,
            our_id: app.rostra_id(),
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
                IncomingConnectionError::Connection { source: _ } => { /* normal, ignore */ }
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
                _ => return UnknownRpcIdSnafu { id }.fail(),
            }
        }
    }

    async fn handle_ping_request(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh_net::endpoint::SendStream,
    ) -> Result<(), IncomingConnectionError> {
        let req = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;
        Connection::write_success_return_code(&mut send).await?;
        Connection::write_message(&mut send, &PingResponse(req.0)).await?;
        Ok(())
    }

    async fn handle_feed_event(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh_net::endpoint::SendStream,
        mut read: iroh_net::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let FeedEventRequest(rostra_core::event::SignedEvent { event, sig }) =
            FeedEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;

        let our_id = self.our_id;

        if event.author != our_id.into() {
            Connection::write_return_code(&mut send, FeedEventResponse::RETURN_CODE_ID_MISMATCH)
                .await?;
            return InvalidRequestSnafu.fail();
        }
        let event_id = event.compute_id();
        event
            .verified_signed_by(sig, our_id)
            .context(InvalidSignatureSnafu)?;

        {
            let app = self.client.app_ref_opt().context(ExitingSnafu)?;

            if app.event_size_limit() < u32::from(event.content_len) {
                app.store_event_too_large(event_id, event).await?;
                Connection::write_return_code(
                    &mut send,
                    FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                )
                .await?;
            }

            if app.does_have_event(event_id).await {
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

        const BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);
        let content_len: u32 = event.content_len.into();
        let ranges = ByteRanges::from(0..content_len.into());
        let chunk_ranges = round_up_to_chunks(&ranges);
        let mut decoded = Vec::with_capacity(content_len.cast_into());
        let mut ob = EmptyOutboard {
            tree: BaoTree::new(content_len.into(), BLOCK_SIZE),
            root: bao_tree::blake3::Hash::from_bytes(event.content_hash.into()),
        };
        bao_tree::io::fsm::decode_ranges(
            TokioStreamReader(&mut read),
            chunk_ranges,
            &mut decoded,
            &mut ob,
        )
        .await
        .context(DecodingBaoSnafu)?;

        {
            let app = self.client.app_ref_opt().context(ExitingSnafu)?;

            app.store_event(event_id, event, decoded.into()).await?;
        }

        Connection::write_success_return_code(&mut send).await?;

        Ok(())
    }

    async fn handle_get_event(
        &self,
        req_msg: Vec<u8>,
        mut send: iroh_net::endpoint::SendStream,
        mut read: iroh_net::endpoint::RecvStream,
    ) -> Result<(), IncomingConnectionError> {
        let GetEventRequest(event_id) =
            GetEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).context(DecodingSnafu)?;

        let client = self.client.app_ref()?;
        let storage = client.storage()?;

        let event = storage.get_event(event_id).await;

        if event.is_some() {
            Connection::write_success_return_code(&mut send).await?;
        } else {
            Connection::write_return_code(&mut send, GetEventRequest::NOT_FOUND).await?;
        }

        todo!();
    }
}
