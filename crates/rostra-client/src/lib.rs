pub mod error;
mod followee_checker;
mod followee_head_checker;
mod head_update_broadcaster;
mod id_publisher;
mod request_handler;

pub mod multiclient;

pub mod id;

use std::str::FromStr;

use error::{
    InvalidDomainSnafu, InvalidEncodingSnafu, InvalidKeySnafu, MissingValueSnafu, RRecordResult,
    WrongTypeSnafu,
};
use futures::future::{self, Either};
use pkarr::dns::rdata::RData;
use pkarr::dns::Name;
use snafu::{OptionExt as _, ResultExt};

const RRECORD_P2P_KEY: &str = "rostra-p2p";
const RRECORD_HEAD_KEY: &str = "rostra-head";
const LOG_TARGET: &str = "rostra";

mod client;
pub use crate::client::*;

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
