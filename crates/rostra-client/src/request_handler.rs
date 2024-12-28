use iroh_net::endpoint::Incoming;
use iroh_net::Endpoint;
use rostra_p2p::connection::{
    Connection, PingRequest, PingResponse, RpcId, RpcIdKnown, RpcMessage as _, MAX_REQUEST_SIZE,
};
use rostra_p2p::RpcError;
use snafu::{OptionExt as _, Report, ResultExt as _, Snafu};
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
    UnknownRequestId {
        id: RpcId,
    },
}
pub type IncomingConnectionResult<T> = std::result::Result<T, IncomingConnectionError>;

pub struct RequestHandler {
    app: ClientHandle,
    endpoint: Endpoint,
}

impl RequestHandler {
    pub fn new(app: &Client, endpoint: Endpoint) -> Self {
        info!(pkarr_id = %app.rostra_id().try_fmt(), "Starting request handler task");
        Self {
            app: app.handle(),
            endpoint,
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

            tokio::spawn(Self::handle_incoming(incoming));
        }
    }
    pub async fn handle_incoming(incoming: Incoming) {
        if let Err(err) = Self::handle_incoming_try(incoming).await {
            info!(target: LOG_TARGET, err=%Report::from_error(err), "Error handling incoming connection");
        }
    }
    pub async fn handle_incoming_try(incoming: Incoming) -> IncomingConnectionResult<()> {
        let conn = incoming
            .accept()
            .context(ConnectionSnafu)?
            .await
            .context(ConnectionSnafu)?;

        loop {
            let (mut send, mut recv) = conn.accept_bi().await.context(ConnectionSnafu)?;
            let (id, content) = Connection::read_request_raw(&mut recv)
                .await
                .context(RpcSnafu)?;

            match id.to_known().context(UnknownRequestIdSnafu { id })? {
                RpcIdKnown::Ping => {
                    let req = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&content)
                        .context(DecodingSnafu)?;
                    Connection::write_message(&mut send, &PingResponse(req.0))
                        .await
                        .context(RpcSnafu)?;
                }
                _ => return UnknownRequestIdSnafu { id }.fail(),
            }
        }
    }
}
