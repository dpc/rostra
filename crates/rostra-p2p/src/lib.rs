pub mod connection;
pub mod error;

use error::BoxedError;
use snafu::Snafu;

pub const ROSTRA_P2P_V0_ALPN: &[u8] = b"rostra-p2p-v0";

#[derive(Debug, Snafu)]
pub enum RpcError {
    Connection {
        source: iroh_net::endpoint::ConnectionError,
    },
    Write {
        source: iroh_net::endpoint::WriteError,
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
}
type RpcResult<T> = std::result::Result<T, RpcError>;