pub mod error;
mod id_publisher;
mod request_handler;

pub mod id;

use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ops;
use std::str::FromStr;
use std::sync::{Arc, Weak};

use error::{IrohError, IrohResult};
use futures::future::{self, Either};
use id::{CompactTicket, IdPublishedData};
use id_publisher::IdPublisher;
use iroh_net::{AddrInfo, NodeAddr};
use itertools::Itertools;
use pkarr::dns::rdata::RData;
use pkarr::dns::{Name, SimpleDnsError};
use pkarr::{Keypair, PkarrClient};
use request_handler::RequestHandler;
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_p2p::connection::Connection;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use snafu::{OptionExt as _, ResultExt, Snafu};
use tracing::debug;

const RRECORD_P2P_KEY: &str = "rostra-p2p";
const RRECORD_TIP_KEY: &str = "rostra-tip";
const LOG_TARGET: &str = "rostra::client";

#[derive(Debug, Snafu)]
pub enum InitError {
    #[snafu(display("Pkarr Client initialization error"))]
    InitPkarrClient { source: pkarr::Error },
    #[snafu(display("Iroh Client initialization error"))]
    InitIrohClient { source: IrohError },
}
pub type InitResult<T> = std::result::Result<T, InitError>;

#[derive(Debug, Snafu)]
pub enum IdResolveError {
    IdNotFound,
    InvalidId { source: pkarr::Error },
    RRecord { source: RRecordError },
    MissingTicket,
    MalformedIrohTicket,
    ConnectionError { source: IrohError },
    PkarrResolve { source: pkarr::Error },
}
type IdResolveResult<T> = std::result::Result<T, IdResolveError>;

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

#[derive(Debug, Snafu)]
pub enum ConnectError {
    Resolve { source: IdResolveError },
    PeerUnavailable,
    ConnectIroh { source: IrohError },
}
pub type ConnectResult<T> = std::result::Result<T, ConnectError>;

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
#[derive(Clone)]
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

pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    pkarr_client: Arc<pkarr::PkarrClientAsync>,
    pkarr_client_relay: Arc<pkarr::PkarrRelayClientAsync>,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    id_keypair: pkarr::Keypair,

    id: RostraId,

    /// Our iroh-net endpoint
    endpoint: iroh_net::Endpoint,
}

#[bon::bon]
impl Client {
    #[builder(finish_fn(name = "build"))]
    pub async fn new(
        #[builder(default = true)] start_request_handler: bool,
        #[builder(default = true)] start_id_publisher: bool,
    ) -> InitResult<Arc<Self>> {
        let id_keypair = Keypair::random();
        let id = RostraId::from(id_keypair.clone());

        debug!(id = %id.try_fmt(), "Rostra Client");

        let pkarr_client = PkarrClient::builder()
            .build()
            .context(InitPkarrClientSnafu)?
            .as_async()
            .into();

        let pkarr_client_relay = pkarr::PkarrRelayClient::new(pkarr::RelaySettings {
            relays: vec!["https://dns.iroh.link/pkarr".to_string()],
            ..pkarr::RelaySettings::default()
        })
        .expect("Has a relay")
        .as_async()
        .into();

        let endpoint = Self::make_iroh_endpoint().await?;

        let client = Arc::new_cyclic(|app| Self {
            handle: app.clone().into(),
            id_keypair,
            endpoint,
            pkarr_client,
            pkarr_client_relay,
            id,
        });

        if start_request_handler {
            client.start_request_handler();
        }

        if start_id_publisher {
            client.start_id_publisher();
        }

        Ok(client)
    }

    pub fn rostra_id(&self) -> RostraId {
        self.id
    }

    pub async fn connect(&self, id: RostraId) -> ConnectResult<Connection> {
        let conn_data = self.resolve_id_data(id).await.context(ResolveSnafu)?;

        let ticket = conn_data.ticket.context(PeerUnavailableSnafu)?;

        Ok(self
            .endpoint
            .connect(ticket, ROSTRA_P2P_V0_ALPN)
            .await
            .context(ConnectIrohSnafu)?
            .into())
    }

    pub(crate) async fn make_iroh_endpoint() -> InitResult<iroh_net::Endpoint> {
        use iroh_net::key::SecretKey;
        use iroh_net::Endpoint;

        let secret_key = SecretKey::generate();
        let ep = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            // We rely entirely on tickets publicshed by our own publisher
            // for every RostraID via Pkarr, so we don't need discovery
            // .discovery(Box::new(discovery))
            .bind()
            .await
            .context(InitIrohClientSnafu)?;
        Ok(ep)
    }

    fn start_id_publisher(&self) {
        tokio::spawn(IdPublisher::new(self, self.id_keypair.clone()).run());
    }

    fn start_request_handler(&self) {
        tokio::spawn(RequestHandler::new(self, self.endpoint.clone()).run());
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

    pub async fn event_tip(&self) -> Option<ShortEventId> {
        // TODO
        None
    }

    pub(crate) fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    pub async fn resolve_id_data(&self, id: RostraId) -> IdResolveResult<IdPublishedData> {
        let public_key = pkarr::PublicKey::try_from(id).context(InvalidIdSnafu)?;
        let domain = public_key.to_string();
        let packet = take_first_ok_some(
            self.pkarr_client.resolve(&public_key),
            self.pkarr_client_relay.resolve(&public_key),
        )
        .await
        .context(PkarrResolveSnafu)?
        .context(IdNotFoundSnafu)?;

        let ticket = get_rrecord_typed(&packet, &domain, RRECORD_P2P_KEY).context(RRecordSnafu)?;
        let tip = get_rrecord_typed(&packet, &domain, RRECORD_TIP_KEY).context(RRecordSnafu)?;

        debug!(target: LOG_TARGET, id = %id.try_fmt(), ticket = ?ticket, tip = ?tip, "Resolved Id");

        Ok(IdPublishedData { ticket, tip })
    }

    pub async fn resolve_id_ticket(&self, id: RostraId) -> IdResolveResult<CompactTicket> {
        self.resolve_id_data(id)
            .await?
            .ticket
            .context(MissingTicketSnafu)
    }

    pub async fn fetch_data(&self, id: RostraId) -> IdResolveResult<String> {
        let data = self.resolve_id_data(id).await?;

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

    fn pkarr_client_relay(&self) -> Arc<pkarr::PkarrRelayClientAsync> {
        self.pkarr_client_relay.clone()
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

async fn take_first_ok_some<T, E, F1, F2>(fut1: F1, fut2: F2) -> Result<Option<T>, E>
where
    F1: future::Future<Output = Result<Option<T>, E>>,
    F2: future::Future<Output = Result<Option<T>, E>>,
{
    let fut1 = Box::pin(fut1);
    let fut2 = Box::pin(fut2);

    match future::select(fut1, fut2).await {
        Either::Left((ok @ Ok(Some(_)), _)) => ok,
        Either::Left((_ok @ Ok(None), fut2)) => {
            // TODO: reconsider?
            fut2.await
        }
        Either::Left((Err(_), fut2)) => fut2.await,
        Either::Right((ok @ Ok(Some(_)), _)) => ok,
        Either::Right((_ok @ Ok(None), fut1)) => {
            // TODO: reconsider
            fut1.await
        }
        Either::Right((Err(_), fut1)) => fut1.await,
    }
}
