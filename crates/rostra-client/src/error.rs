use std::io;

use pkarr::dns::SimpleDnsError;
use rostra_client_db::DbError;
use rostra_core::event::ContentValidationError;
use rostra_core::id::{RostraId, RostraIdSecretKeyError};
use rostra_core::{ShortEventId, Timestamp};
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
    #[snafu(display("Pkarr relay HTTP client initialization error"))]
    InitPkarrRelayHttpClient { source: reqwest_13::Error },
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
    /// Shutdown permanently prevents activation of this client.
    #[snafu(display("Client shutdown has started"))]
    ActivationClosing,
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
    /// Canonical input was rejected before any candidate signature.
    #[cfg(feature = "native-media")]
    #[snafu(display("Normalized media rejected: {source}"))]
    StaticMedia {
        source: rostra_core::static_media::StaticMediaError,
    },
    /// Shutdown permanently prevents new signing operations.
    #[snafu(display("Client shutdown has started"))]
    ClientClosing,
    /// No event was signed: the local follow queue/clock wait budget expired.
    /// The caller may explicitly retry; publication is never retried
    /// internally.
    #[snafu(display("Follow ordering is busy; retry the operation"))]
    FollowOrderingBusy,
    /// No event was signed: a retained same-author follow is ahead of the
    /// clock. Check the device clock or the other signing device before
    /// retrying.
    #[snafu(display("Follow timestamp {latest} is ahead of the device clock {now}"))]
    FollowClockBehind { latest: Timestamp, now: Timestamp },
    /// No strictly greater timestamp can follow the retained boundary.
    #[snafu(display("Follow timestamp ordering is exhausted"))]
    FollowTimestampExhausted,
    /// No event was signed before the profile queue/clock budget expired.
    #[snafu(display("Profile ordering is busy; retry the operation"))]
    ProfileOrderingBusy,
    /// A retained profile is ahead of wall time; no event was signed.
    #[snafu(display("Profile timestamp {latest} is ahead of the device clock {now}"))]
    ProfileClockBehind { latest: Timestamp, now: Timestamp },
    /// No strictly greater profile timestamp can be represented.
    #[snafu(display("Profile timestamp ordering is exhausted"))]
    ProfileTimestampExhausted,
    #[snafu(display("Vote ordering wait budget exhausted"))]
    VoteOrderingBusy,
    #[snafu(display("Vote clock behind retained winner: {now} < {latest}"))]
    VoteClockBehind { latest: Timestamp, now: Timestamp },
    #[snafu(display("Vote singleton timestamp is exhausted"))]
    VoteTimestampExhausted,
    #[snafu(display("Direct messages require an unlocked full-account client"))]
    DirectMessageUnavailable,
    #[snafu(display("Direct-message encoding failed: {source}"))]
    DirectMessageEncoding { source: rostra_dm::Error },
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
