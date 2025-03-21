use core::{fmt, str};

use iroh::NodeAddr;
use iroh_base::ticket::{NodeTicket, Ticket as _};
use rostra_core::ShortEventId;
use rostra_util_error::BoxedError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactTicket(pub NodeTicket);

impl fmt::Display for CompactTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        data_encoding::BASE64URL_NOPAD.encode_write(&self.0.to_bytes(), f)
    }
}

impl str::FromStr for CompactTicket {
    type Err = BoxedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::BASE64URL_NOPAD.decode(s.as_bytes())?;
        Ok(Self(NodeTicket::from_bytes(&bytes)?))
    }
}

impl From<NodeAddr> for CompactTicket {
    fn from(addr: NodeAddr) -> Self {
        Self(NodeTicket::from(addr))
    }
}

impl From<CompactTicket> for NodeAddr {
    fn from(val: CompactTicket) -> Self {
        val.0.into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdPublishedData {
    pub ticket: Option<CompactTicket>,
    pub head: Option<ShortEventId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdResolvedData {
    pub published: IdPublishedData,
    pub timestamp: u64,
}
