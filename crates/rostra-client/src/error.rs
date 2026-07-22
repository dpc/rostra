use std::io;

use pkarr::dns::SimpleDnsError;
use rostra_client_db::DbError;
use rostra_core::ShortEventId;
use rostra_core::event::ContentValidationError;
use rostra_core::id::{RostraId, RostraIdSecretKeyError};
use rostra_util_error::BoxedError;
use snafu::Snafu;

/// Meh alias
pub type IrohError = anyhow::Error;
pub type IrohResult<T> = anyhow::Result<T>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum InitError {
    #[snafu(display("Pkarr Client initialization error"))]
    InitPkarrClient { source: pkarr::errors::BuildError },
    #[snafu(display("Iroh Client initialization error"))]
    InitIrohClient { source: iroh::endpoint::BindError },
    #[snafu(display("Failed to activate"))]
    Activate { source: ActivateError },
    #[snafu(transparent)]
    Db { source: DbError },
}

pub type InitResult<T> = std::result::Result<T, InitError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum ActivateError {
    #[snafu(display("Secret key does not match RostraId"))]
    SecretMismatch,
    #[snafu(display("Failed to store the local node announcement: {source}"))]
    LocalAnnouncementStorage { source: PostError },
}

pub type ActivateResult<T> = std::result::Result<T, ActivateError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum IdResolveError {
    NotFound,
    InvalidId {
        source: pkarr::errors::PublicKeyError,
    },
    RRecord {
        source: RRecordError,
    },
    MissingTicket,
    MalformedIrohTicket,
    PkarrResolve,
}

pub(crate) type IdResolveResult<T> = std::result::Result<T, IdResolveError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum IdPublishError {
    PkarrSignedPacket {
        source: pkarr::errors::SignedPacketBuildError,
    },
    PkarrPublish {
        source: pkarr::errors::PublishError,
    },
    PkarrPacketBuild {
        source: pkarr::errors::SignedPacketBuildError,
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
    Resolve {
        source: IdResolveError,
    },
    #[snafu(display("Ping failed after connecting: {source}"))]
    PingFailed {
        source: rostra_p2p::RpcError,
    },
    ConnectIroh {
        source: iroh::endpoint::ConnectError,
    },
    #[snafu(display("Node is in backoff, try again later"))]
    NodeInBackoff,
    #[snafu(display("Resolved to own endpoint, cannot connect to self"))]
    ResolvedToSelf,
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
    #[snafu(visibility(pub))]
    Encode { source: BoxedError },
    #[snafu(transparent)]
    Validation { source: ContentValidationError },
    #[snafu(display("Failed to store the published event: {source}"))]
    Storage { source: DbError },
}

pub type PostResult<T> = std::result::Result<T, PostError>;

/// Failure to ingest verified event content at a client request boundary.
#[derive(Debug, Snafu)]
#[snafu(display("Failed to store event {event_id} by {author_id}: {source}"))]
pub struct StoreEventError {
    /// Authenticated author of the event being stored.
    pub author_id: RostraId,
    /// Short identifier of the event being stored.
    pub event_id: ShortEventId,
    /// Database ingestion failure.
    pub source: DbError,
}

/// Result of storing verified event content through the client boundary.
pub type StoreEventResult<T> = std::result::Result<T, StoreEventError>;

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
