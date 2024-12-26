pub mod error;
mod id_publisher;

pub mod id;

use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ops;
use std::str::FromStr;
use std::sync::{Arc, Weak};

use error::{IrohError, IrohResult};
use id::IdPublishedData;
use id_publisher::IdPublisher;
use iroh_net::{AddrInfo, NodeAddr};
use itertools::Itertools;
use pkarr::dns::rdata::RData;
use pkarr::dns::{Name, SimpleDnsError};
use pkarr::{dns, Keypair, PkarrClient};
use rostra_core::event::EventId;
use rostra_core::id::RostraId;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use snafu::{OptionExt as _, ResultExt, Snafu};

const RRECORD_P2P_KEY: &str = "rostra-p2p";
const RRECORD_TIP_KEY: &str = "rostra-tip";
const LOG_TARGET: &str = "rostra::client";

#[derive(Debug, Snafu)]
pub enum InitError {
    // Iroh { source: IrohError },
    #[snafu(display("Pkarr Client initialization error"))]
    PkarrClient { source: pkarr::Error },
    #[snafu(display("Iroh Client initialization error"))]
    IrohClient { source: IrohError },
}
pub type InitResult<T> = std::result::Result<T, InitError>;

#[derive(Debug, Snafu)]
pub enum IdResolveError {
    IdNotFound,
    RRecord { source: RRecordError },
    MissingTicket,
    MalformedIrohTicket,
    ConnectionError { source: IrohError },
    PkarrResolve { source: pkarr::Error },
}

#[derive(Debug, Snafu)]
pub enum IdPublishError {
    PkarrPublish {
        source: pkarr::Error,
    },
    PkarrPacket {
        source: pkarr::Error,
    },
    #[snafu(display("Iroh Client initialization error"))]
    DnsError {
        source: SimpleDnsError,
    },
}
pub type IdPublishResult<T> = std::result::Result<T, IdPublishError>;

type IdResolveResult<T> = std::result::Result<T, IdResolveError>;

/// Weak handle to [`Client`]
#[derive(Debug, Clone)]
pub struct ClientHandle(Weak<Client>);

impl ClientHandle {
    pub fn app_ref(&self) -> Option<ClientRef<'_>> {
        let app = self.0.upgrade()?;
        Some(ClientRef {
            app,
            r: PhantomData,
        })
    }
}

impl From<Weak<Client>> for ClientHandle {
    fn from(value: Weak<Client>) -> Self {
        Self(value)
    }
}

/// A strong reference to [`Client`]
///
/// It contains a phantom reference, to avoid attempts of
/// storing it anywhere.
#[derive(Debug, Clone)]
pub struct ClientRef<'r> {
    pub(crate) app: Arc<Client>,
    pub(crate) r: PhantomData<&'r ()>,
}

impl<'r> ops::Deref for ClientRef<'r> {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

#[derive(Debug)]
pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    pkarr_client: Arc<pkarr::PkarrClientAsync>,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    id_keypair: pkarr::Keypair,

    id: RostraId,

    /// Our iroh-net endpoint
    endpoint: iroh_net::Endpoint,
}

impl Client {
    pub async fn new() -> InitResult<Arc<Self>> {
        let pkarr_client = PkarrClient::builder()
            .build()
            .context(PkarrClientSnafu)?
            .as_async()
            .into();

        let endpoint = Self::make_iroh_endpoint().await?;

        let id_keypair = Keypair::random();
        let id = RostraId::from(id_keypair.public_key());

        let client = Arc::new_cyclic(|app| Self {
            handle: app.clone().into(),
            id_keypair,
            endpoint,
            pkarr_client,
            id,
        });

        client.start_id_publisher();

        Ok(client)
    }

    pub fn rostra_id(&self) -> RostraId {
        self.id
    }

    pub(crate) async fn make_iroh_endpoint() -> InitResult<iroh_net::Endpoint> {
        use iroh_net::discovery::dns::DnsDiscovery;
        use iroh_net::discovery::pkarr::PkarrPublisher;
        use iroh_net::discovery::ConcurrentDiscovery;
        use iroh_net::key::SecretKey;
        use iroh_net::Endpoint;

        let secret_key = SecretKey::generate();
        let discovery = ConcurrentDiscovery::from_services(vec![
            Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
            Box::new(DnsDiscovery::n0_dns()),
        ]);
        let ep = Endpoint::builder()
            .secret_key(secret_key)
            .discovery(Box::new(discovery))
            .bind()
            .await
            .context(IrohClientSnafu)?;
        Ok(ep)
    }

    fn start_id_publisher(&self) {
        tokio::spawn(IdPublisher::new(self, self.id_keypair.clone()).run());
    }

    pub(crate) async fn iroh_address(&self) -> IrohResult<NodeAddr> {
        fn sanitize_addr_info(addr_info: AddrInfo) -> AddrInfo {
            fn is_ipv4_cgnat(ip: Ipv4Addr) -> bool {
                matches!(ip.octets(), [100, b, ..] if (64..128).contains(&b))
            }
            let direct_addresses = addr_info
                .direct_addresses
                .into_iter()
                .filter(|addr| match addr {
                    std::net::SocketAddr::V4(ipv4) => {
                        let ip = ipv4.ip();
                        !ip.is_private()
                            && !ip.is_link_local()
                            && !is_ipv4_cgnat(*ip)
                            && !ip.is_loopback()
                            && !ip.is_multicast()
                            && !ip.is_broadcast()
                            && !ip.is_documentation()
                    }
                    std::net::SocketAddr::V6(ipv6) => {
                        let ip = ipv6.ip();
                        !ip.is_multicast()
                            && !ip.is_loopback()
                            // Unique Local Addresses (ULA)
                            && (ip.to_bits() & !0x7f) != 0xfc00_0000_0000_0000_0000_0000_0000_0000
                            // Link-Local Addresses
                            && (ip.to_bits() & !0x3ff) != 0xfe80_0000_0000_0000_0000_0000_0000_0000
                    }
                })
                .unique_by(|addr| match addr.ip() {
                    IpAddr::V4(ipv4) => IpAddr::V4(ipv4),
                    IpAddr::V6(ipv6) => IpAddr::V6(Ipv6Addr::from_bits(
                        ipv6.to_bits() & !0xffff_ffff_ffff_ffffu128,
                    )),
                })
                .sorted_unstable_by(|a, b| a.is_ipv6().cmp(&b.is_ipv6()).then(a.cmp(b)))
                // Limit to 4
                .take(4)
                .collect();
            AddrInfo {
                direct_addresses,
                ..addr_info
            }
        }

        fn sanitize_node_addr(node_addr: NodeAddr) -> NodeAddr {
            NodeAddr {
                info: sanitize_addr_info(node_addr.info),
                ..node_addr
            }
        }
        self.endpoint.node_addr().await.map(sanitize_node_addr)
    }

    pub async fn event_tip(&self) -> Option<EventId> {
        // TODO
        None
    }

    pub(crate) fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    pub async fn resolve_id(&self, id: RostraId) -> IdResolveResult<IdPublishedData> {
        let domain = id.to_string();
        let packet = self
            .pkarr_client
            .resolve(&id.into())
            .await
            .context(PkarrResolveSnafu)?
            .context(IdNotFoundSnafu)?;

        let ticket = get_rrecord_typed(&packet, &domain, RRECORD_P2P_KEY).context(RRecordSnafu)?;
        let tip = get_rrecord_typed(&packet, &domain, RRECORD_TIP_KEY).context(RRecordSnafu)?;

        Ok(IdPublishedData { ticket, tip })
    }

    pub async fn fetch_data(&self, id: RostraId) -> IdResolveResult<String> {
        let data = self.resolve_id(id).await?;

        let ticket = data.ticket.context(MissingTicketSnafu)?;
        let _connection = self
            .endpoint
            .connect(ticket, ROSTRA_P2P_V0_ALPN)
            .await
            .context(ConnectionSnafu)?;

        todo!()
    }

    pub(crate) fn pkarr_client(&self) -> Arc<pkarr::PkarrClientAsync> {
        self.pkarr_client.clone()
    }
}

#[derive(Debug, Snafu)]
pub enum RRecordError {
    MissingRecord,
    WrongType,
    MissingValue,
    // TODO: InvalidEncoding { source: BoxedError },
    InvalidEncoding,
    InvalidKey { source: SimpleDnsError },
    InvalidDomain { source: SimpleDnsError },
}
type RRecordResult<T> = Result<T, RRecordError>;

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
        .packet()
        .answers
        .iter()
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
