use std::io;

use pkarr::dns::SimpleDnsError;
use rostra_core::id::RostraIdSecretKeyError;
use snafu::{Snafu, Whatever};

use crate::db::DbError;

/// Meh alias
pub type IrohError = anyhow::Error;
pub type IrohResult<T> = anyhow::Result<T>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum InitError {
    #[snafu(display("Pkarr Client initialization error"))]
    InitPkarrClient { source: pkarr::Error },
    #[snafu(display("Iroh Client initialization error"))]
    InitIrohClient { source: IrohError },
    #[snafu(transparent)]
    Db { source: DbError },
}

pub type InitResult<T> = std::result::Result<T, InitError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum IdResolveError {
    NotFound,
    InvalidId { source: pkarr::Error },
    RRecord { source: RRecordError },
    MissingTicket,
    MalformedIrohTicket,
    PkarrResolve { source: pkarr::Error },
}

pub(crate) type IdResolveResult<T> = std::result::Result<T, IdResolveError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum IdPublishError {
    PkarrPublish {
        source: pkarr::Error,
    },
    PkarrPacket {
        source: pkarr::Error,
    },
    #[snafu(display("Iroh Client initialization error"))]
    Dns {
        source: SimpleDnsError,
    },
}

pub type IdPublishResult<T> = std::result::Result<T, IdPublishError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum ConnectError {
    Resolve { source: IdResolveError },
    PeerUnavailable,
    ConnectIroh { source: IrohError },
}

pub type ConnectResult<T> = std::result::Result<T, ConnectError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum IdSecretReadError {
    Io { source: io::Error },
    Parsing { source: RostraIdSecretKeyError },
}

pub type IdSecretReadResult<T> = std::result::Result<T, IdSecretReadError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum PostError {
    #[snafu(transparent)]
    Resolve { source: IdResolveError },
    #[snafu(display("Encoding error: {source}"))]
    Encode { source: Whatever },
}

pub type PostResult<T> = std::result::Result<T, PostError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum RRecordError {
    MissingRecord,
    WrongType,
    MissingValue,
    // TODO: InvalidEncoding { source: BoxedError },
    InvalidEncoding,
    InvalidKey { source: SimpleDnsError },
    InvalidDomain { source: SimpleDnsError },
}
pub type RRecordResult<T> = Result<T, RRecordError>;
