pub mod connection;
pub mod error;
pub mod util;

pub use connection::Connection;
use rostra_core::event::VerifiedEventError;
use rostra_util_error::BoxedError;
use snafu::Snafu;
pub const ROSTRA_P2P_V0_ALPN: &[u8] = b"rostra-p2p-v0";

pub const LOG_TARGET: &str = "rostra::p2p";

#[derive(Debug, Snafu)]
pub enum RpcError {
    #[snafu(visibility(pub))]
    Connection {
        source: anyhow::Error,
    },
    StreamConnection {
        source: iroh::endpoint::ConnectionError,
    },
    Write {
        source: iroh::endpoint::WriteError,
    },
    Read {
        source: BoxedError,
    },
    MessageTooLarge {
        len: u32,
        limit: u32,
    },
    Decoding {
        source: bincode::error::DecodeError,
    },
    DecodingBao {
        source: bao_tree::io::DecodeError,
    },
    EncodingBao {
        source: bao_tree::io::EncodeError,
    },
    Trailer {
        source: BoxedError,
    },
    EventVerification {
        source: VerifiedEventError,
    },
    /// Other side responded with rpc failure
    Failed {
        return_code: u8,
    },
}
type RpcResult<T> = std::result::Result<T, RpcError>;
