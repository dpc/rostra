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
    Connection, FeedEventRequest, PingRequest, PingResponse, RpcId, RpcIdKnown, RpcMessage as _,
    MAX_REQUEST_SIZE,
};
use rostra_p2p::RpcError;
use rostra_util_error::FmtCompact as _;
use snafu::{ensure, OptionExt as _, ResultExt as _, Snafu};
use tracing::{debug, info, instrument};

use crate::{Client, ClientHandle};

const LOG_TARGET: &str = "rostra::client::req_handler";

#[derive(Debug, Snafu)]
pub enum IncomingConnectionError {
    Connection {
        source: iroh_net::endpoint::ConnectionError,
    },
    Rpc {
        source: RpcError,
    },
    Decoding {
        source: bincode::error::DecodeError,
    },
    DecodingBao {
        source: bao_tree::io::DecodeError,
    },
    // TODO: more details
    InvalidRequest,
    InvalidSignature {
        source: ed25519_dalek::SignatureError,
    },
    UnknownRpcId {
        id: RpcId,
    },
}
pub type IncomingConnectionResult<T> = std::result::Result<T, IncomingConnectionError>;

pub struct RequestHandler {
    app: ClientHandle,
    endpoint: Endpoint,
    inner: Arc<RequestHandlerInner>,
}
pub struct RequestHandlerInner {
    our_id: RostraId,
}

impl RequestHandler {
    pub fn new(app: &Client, endpoint: Endpoint) -> Self {
        info!(pkarr_id = %app.rostra_id().try_fmt(), "Starting request handler task");
        Self {
            app: app.handle(),
            endpoint,
            inner: RequestHandlerInner {
                our_id: app.rostra_id(),
            }
            .into(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        loop {
            if self.app.app_ref().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            };
            let Some(incoming) = self.endpoint.accept().await else {
                debug!(target: LOG_TARGET, "Can't accept any more connection, quitting");
                return;
            };

            let inner = self.inner.clone();
            tokio::spawn(inner.handle_incoming(incoming));
        }
    }
}

impl RequestHandlerInner {
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
            let (id, content) = Connection::read_request_raw(&mut recv)
                .await
                .context(RpcSnafu)?;

            match id.to_known().context(UnknownRpcIdSnafu { id })? {
                RpcIdKnown::Ping => {
                    handle_ping_request(content, send).await?;
                }
                RpcIdKnown::FeedEvent => {
                    handle_feed_event(self.our_id.into(), content, send, recv).await?;
                }
                _ => return UnknownRpcIdSnafu { id }.fail(),
            }
        }
    }
}

async fn handle_ping_request(
    content: Vec<u8>,
    mut send: iroh_net::endpoint::SendStream,
) -> Result<(), IncomingConnectionError> {
    let req = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&content).context(DecodingSnafu)?;
    Connection::write_message(&mut send, &PingResponse(req.0))
        .await
        .context(RpcSnafu)?;
    Ok(())
}

async fn handle_feed_event(
    our_id: RostraId,
    content: Vec<u8>,
    mut send: iroh_net::endpoint::SendStream,
    mut read: iroh_net::endpoint::RecvStream,
) -> Result<(), IncomingConnectionError> {
    let FeedEventRequest { event, sig } =
        FeedEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&content).context(DecodingSnafu)?;

    ensure!(event.author != our_id.into(), InvalidRequestSnafu);
    event
        .verified_signed_by(sig, our_id)
        .context(InvalidSignatureSnafu)?;
    // TODO: verify signature, etc.

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

    // write somewhere
    todo!("send some bytes to ack?")
}
