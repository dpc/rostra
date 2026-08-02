//! A runtime client for one Rostra identity.
//!
//! [`Client`] is the supported entry point. Construct it with [`Client::builder`]:
//! omitting [`Database`] creates a temporary in-memory, light client. Supplying
//! a durable database creates a full client and enables replication and
//! projection tasks. Clients start read-only unless the builder receives a
//! [`RostraIdSecretKey`] or [`Client::unlock_active`] is called later.
//!
//! Client-created transports default to relay-only mode so that creating a
//! client does not expose its host's IP address. Call the builder's
//! `public_mode(true)` only when direct IP connectivity is an intentional
//! privacy tradeoff. A caller that supplies `iroh_endpoint` owns that
//! endpoint's transport and privacy policy. The request handler is enabled by
//! default in both storage modes; durable clients also start background
//! synchronization by default.
//!
//! A client owns its background tasks. Clone the returned [`std::sync::Arc`] to
//! keep the runtime alive; dropping the final strong reference aborts those
//! tasks. [`ClientHandle`] is weak and does not extend the runtime's lifetime.
//! There is no separate shutdown protocol.
//!
//! Initialization, activation, peer connection, publication, and explicit
//! synchronization return typed errors from [`error`] or [`DbError`]. A
//! database failure in background ingestion is logged and stops the affected
//! worker; the client does not globally restart failed workers. Database access
//! remains available through [`Client::db`] for integrations that need Rostra's
//! materialized views.

pub mod error;

mod connection_cache;
pub(crate) mod task;

pub mod multiclient;

pub mod id;

mod util;

use std::str::FromStr;

use error::{
    InvalidDomainSnafu, InvalidEncodingSnafu, InvalidKeySnafu, MissingValueSnafu, RRecordResult,
    WrongTypeSnafu,
};
use futures::future::{self, Either};
use pkarr::dns::Name;
use pkarr::dns::rdata::RData;
use snafu::{OptionExt as _, ResultExt};

const RRECORD_P2P_KEY: &str = "rostra-p2p";
const RRECORD_HEAD_KEY: &str = "rostra-head";
const LOG_TARGET: &str = "rostra";

mod client;
mod net;
pub use rostra_client_db::{
    Database, DbError, SOCIAL_POST_MATERIALIZATION_SCAN_MAX, SelfFollowee,
    SocialPostMaterialization, SocialPostMaterializationCursor, SocialPostMaterializationPage,
};
pub use rostra_core::id::{RostraId, RostraIdSecretKey};
pub use rostra_core::{ExternalEventId, ShortEventId};

pub use crate::client::{
    Client, ClientHandle, ClientRef, ClientRefError, ClientRefResult, IdP2PState, NodeP2PState,
    NodeSource, P2PState,
};
pub use crate::id::{CompactTicket, IdPublishedData, IdResolvedData};
pub use crate::multiclient::{MultiClient, MultiClientError, MultiClientResult};

fn get_rrecord_typed<T>(
    packet: &pkarr::SignedPacket,
    domain: &str,
    key: &str,
) -> RRecordResult<Option<T>>
where
    T: FromStr,
    // <T as FromStr>::Err: std::error::Error + Send + Sync + 'static,
{
    get_rrecord(packet, domain, key)?
        .as_deref()
        .map(T::from_str)
        .transpose()
        .ok()
        .context(InvalidEncodingSnafu)
}

fn get_rrecord(
    packet: &pkarr::SignedPacket,
    domain: &str,
    key: &str,
) -> RRecordResult<Option<String>> {
    let domain = Name::new(domain).context(InvalidDomainSnafu)?;
    let key = Name::new(key).context(InvalidKeySnafu)?;
    let value = match packet
        .all_resource_records()
        .find(|a| a.name.without(&domain).is_some_and(|sub| sub == key))
        .map(|r| r.rdata.to_owned())
    {
        Some(RData::TXT(value)) => value,
        Some(_) => WrongTypeSnafu.fail()?,
        None => return Ok(None),
    };
    let v = value
        .attributes()
        .into_keys()
        .next()
        .context(MissingValueSnafu)?;
    Ok(Some(v))
}

// Generic function that takes two futures and returns the first Ok result
#[allow(dead_code)]
async fn take_first_ok<T, E, F1, F2>(fut1: F1, fut2: F2) -> Result<T, E>
where
    F1: future::Future<Output = Result<T, E>>,
    F2: future::Future<Output = Result<T, E>>,
{
    let fut1 = Box::pin(fut1);
    let fut2 = Box::pin(fut2);

    match future::select(fut1, fut2).await {
        Either::Left((ok @ Ok(_), _)) => ok,
        Either::Left((Err(_), fut2)) => fut2.await,
        Either::Right((ok @ Ok(_), _)) => ok,
        Either::Right((Err(_), fut1)) => fut1.await,
    }
}
