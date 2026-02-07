use core::{fmt, str};

use iroh_base::{EndpointAddr, EndpointId};
use rostra_core::ShortEventId;
use rostra_core::event::IrohNodeId;
use rostra_util_error::BoxedError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactTicket(pub EndpointId);

impl CompactTicket {
    /// Returns the endpoint ID as an IrohNodeId.
    pub fn to_iroh_node_id(&self) -> IrohNodeId {
        IrohNodeId::from_bytes(*self.0.as_bytes())
    }
}

impl fmt::Display for CompactTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        data_encoding::BASE64URL_NOPAD.encode_write(self.0.as_bytes(), f)
    }
}

impl str::FromStr for CompactTicket {
    type Err = BoxedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::BASE64URL_NOPAD.decode(s.as_bytes())?;
        let id = EndpointId::try_from(&bytes[..])?;
        Ok(Self(id))
    }
}

impl From<EndpointId> for CompactTicket {
    fn from(id: EndpointId) -> Self {
        Self(id)
    }
}

impl From<EndpointAddr> for CompactTicket {
    fn from(addr: EndpointAddr) -> Self {
        Self(addr.id)
    }
}

impl From<CompactTicket> for EndpointAddr {
    fn from(val: CompactTicket) -> Self {
        EndpointAddr::new(val.0)
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
