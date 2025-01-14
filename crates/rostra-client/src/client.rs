use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::str::FromStr as _;
use std::sync::{Arc, Weak};
use std::time::Duration;
use std::{ops, result};

use backon::Retryable as _;
use iroh_net::{AddrInfo, NodeAddr};
use itertools::Itertools as _;
use pkarr::PkarrClient;
use rostra_core::event::{Event, EventContent, EventKindKnown, SignedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_core::ShortEventId;
use rostra_p2p::connection::{Connection, FeedEventRequest, FeedEventResponse};
use rostra_p2p::RpcError;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::FmtCompact as _;
use rostra_util_fmt::AsFmtOption as _;
use snafu::{OptionExt as _, ResultExt as _, Snafu};
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::{debug, info};

use super::{get_rrecord_typed, take_first_ok_some, RRECORD_HEAD_KEY, RRECORD_P2P_KEY};
use crate::db::{Database, DbResult};
use crate::error::{
    ConnectIrohSnafu, ConnectResult, IdResolveError, IdResolveResult, IdSecretReadResult,
    InitIrohClientSnafu, InitPkarrClientSnafu, InitResult, InvalidIdSnafu, IoSnafu, IrohResult,
    MissingTicketSnafu, NotFoundSnafu, ParsingSnafu, PkarrResolveSnafu, PostResult, RRecordSnafu,
    ResolveSnafu,
};
use crate::id::{CompactTicket, IdPublishedData, IdResolvedData};
use crate::id_publisher::IdPublisher;
use crate::request_handler::RequestHandler;
use crate::storage::Storage;
use crate::LOG_TARGET;

#[derive(Debug, Snafu)]
pub struct ClientRefError;

pub type ClientRefResult<T> = Result<T, ClientRefError>;

#[derive(Debug)]
pub enum ClientMode {
    Full(Database),
    Light,
}

impl ClientMode {
    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full(_))
    }
}
/// Weak handle to [`Client`]
#[derive(Debug, Clone)]
pub struct ClientHandle(Weak<Client>);

impl ClientHandle {
    pub fn app_ref_opt(&self) -> Option<ClientRef<'_>> {
        let client = self.0.upgrade()?;
        Some(ClientRef {
            client,
            r: PhantomData,
        })
    }
    pub fn app_ref(&self) -> ClientRefResult<ClientRef<'_>> {
        let client = self.0.upgrade().context(ClientRefSnafu)?;
        Ok(ClientRef {
            client,
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
    pub(crate) client: Arc<Client>,
    pub(crate) r: PhantomData<&'r ()>,
}

impl<'r> ops::Deref for ClientRef<'r> {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    pub(crate) pkarr_client: Arc<pkarr::PkarrClientAsync>,
    pub(crate) pkarr_client_relay: Arc<pkarr::PkarrRelayClientAsync>,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    pub(crate) id: RostraId,
    pub(crate) id_secret: RostraIdSecretKey,

    storage: Option<Arc<Storage>>,

    /// Our iroh-net endpoint
    ///
    /// Each time new random-seed generated one, (optionally) published via
    /// Pkarr under main `RostraId` identity.
    pub(crate) endpoint: iroh_net::Endpoint,

    /// A watch-channel that can be used to notify some tasks manually to check
    /// for updates again
    check_for_updates_tx: watch::Sender<()>,
}

#[derive(Debug, Snafu)]
#[snafu(display("Client storage not available"))]
pub struct ClientStorageError;

pub type ClientStorageResult<T> = result::Result<T, ClientStorageError>;

#[bon::bon]
impl Client {
    #[builder(finish_fn(name = "build"))]
    pub async fn new(
        id_secret: Option<RostraIdSecretKey>,
        #[builder(default = true)] start_request_handler: bool,
        #[builder(default = true)] start_id_publisher: bool,

        #[builder(default = ClientMode::Light)] // Default to light
        mode: ClientMode,
    ) -> InitResult<Arc<Self>> {
        let id_secret = id_secret.unwrap_or_else(|| RostraIdSecretKey::generate());
        let id = id_secret.id();

        debug!(id = %id.try_fmt(), "Rostra Client");

        let is_mode_full = mode.is_full();

        let storage = match mode {
            ClientMode::Full(db) => {
                let storage = Storage::new(db, id).await?;
                Some(storage)
            }
            ClientMode::Light => None,
        };

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
        let (check_for_updates_tx, _) = watch::channel(());

        let client = Arc::new_cyclic(|client| Self {
            handle: client.clone().into(),
            id_secret,
            endpoint,
            pkarr_client,
            pkarr_client_relay,
            storage: storage.map(Into::into),
            id,
            check_for_updates_tx,
        });

        if start_request_handler {
            client.start_request_handler();
        }

        if start_id_publisher {
            client.start_id_publisher();
        }

        if is_mode_full {
            client.start_followee_checker();
            client.start_followee_head_checker();
        }

        Ok(client)
    }
}

impl Client {
    pub fn rostra_id(&self) -> RostraId {
        self.id
    }

    pub async fn connect(&self, id: RostraId) -> ConnectResult<Connection> {
        let ticket = self.resolve_id_ticket(id).await.context(ResolveSnafu)?;

        Ok(self
            .endpoint
            .connect(ticket, ROSTRA_P2P_V0_ALPN)
            .await
            .context(ConnectIrohSnafu)?
            .into())
    }

    pub async fn connect_ticket(&self, ticket: CompactTicket) -> ConnectResult<Connection> {
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

    pub(crate) fn start_id_publisher(&self) {
        tokio::spawn(IdPublisher::new(self, self.id_secret.clone()).run());
    }

    pub(crate) fn start_request_handler(&self) {
        tokio::spawn(RequestHandler::new(self, self.endpoint.clone()).run());
    }

    pub(crate) fn start_followee_checker(&self) {
        tokio::spawn(crate::followee_checker::FolloweeChecker::new(self).run());
    }
    pub(crate) fn start_followee_head_checker(&self) {
        tokio::spawn(crate::followee_head_checker::FolloweeHeadChecker::new(self).run());
    }

    pub(crate) async fn iroh_address(&self) -> IrohResult<NodeAddr> {
        pub(crate) fn sanitize_addr_info(addr_info: AddrInfo) -> AddrInfo {
            pub(crate) fn is_ipv4_cgnat(ip: Ipv4Addr) -> bool {
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

        pub(crate) fn sanitize_node_addr(node_addr: NodeAddr) -> NodeAddr {
            NodeAddr {
                info: sanitize_addr_info(node_addr.info),
                ..node_addr
            }
        }
        self.endpoint.node_addr().await.map(sanitize_node_addr)
    }

    pub async fn events_head(&self) -> Option<ShortEventId> {
        // TODO
        None
    }

    pub(crate) fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    pub async fn resolve_id_data(&self, id: RostraId) -> IdResolveResult<IdResolvedData> {
        let public_key = pkarr::PublicKey::try_from(id).context(InvalidIdSnafu)?;
        let domain = public_key.to_string();
        let packet = take_first_ok_some(
            self.pkarr_client.resolve(&public_key),
            self.pkarr_client_relay.resolve(&public_key),
        )
        .await
        .context(PkarrResolveSnafu)?
        .context(NotFoundSnafu)?;

        let timestamp = packet.timestamp();
        let ticket = get_rrecord_typed(&packet, &domain, RRECORD_P2P_KEY).context(RRecordSnafu)?;
        let head = get_rrecord_typed(&packet, &domain, RRECORD_HEAD_KEY).context(RRecordSnafu)?;

        debug!(
            target: LOG_TARGET,
            id = %id.try_fmt(),
            ticket = %ticket.fmt_option(),
            head=%head.fmt_option(),
            "Resolved Id"
        );

        Ok(IdResolvedData {
            published: IdPublishedData { ticket, head },
            timestamp,
        })
    }

    pub async fn resolve_id_ticket(&self, id: RostraId) -> IdResolveResult<CompactTicket> {
        self.resolve_id_data(id)
            .await?
            .published
            .ticket
            .context(MissingTicketSnafu)
    }

    pub(crate) fn pkarr_client(&self) -> Arc<pkarr::PkarrClientAsync> {
        self.pkarr_client.clone()
    }

    pub(crate) fn pkarr_client_relay(&self) -> Arc<pkarr::PkarrRelayClientAsync> {
        self.pkarr_client_relay.clone()
    }

    pub(crate) async fn does_have_event(&self, _event_id: rostra_core::EventId) -> bool {
        // TODO: check
        false
    }

    pub async fn store_event(
        &self,
        _event_id: impl Into<ShortEventId>,
        event: Event,
        content: EventContent,
    ) -> DbResult<()> {
        // TODO: store
        info!(target: LOG_TARGET, ?event, ?content, "Pretending to store");
        Ok(())
    }

    pub async fn store_event_too_large(
        &self,
        _event_id: impl Into<ShortEventId>,
        _event: Event,
    ) -> DbResult<()> {
        unimplemented!()
    }

    pub(crate) fn event_size_limit(&self) -> u32 {
        // TODO: take from db or something
        16 * 1024 * 1024
    }

    pub async fn read_id_secret(path: &Path) -> IdSecretReadResult<RostraIdSecretKey> {
        let content = tokio::fs::read_to_string(path).await.context(IoSnafu)?;
        RostraIdSecretKey::from_str(&content).context(ParsingSnafu)
    }

    pub async fn check_published_id_state(&self) -> IdResolveResult<IdResolvedData> {
        (|| async { self.resolve_id_data(self.id).await })
            .retry(
                backon::FibonacciBuilder::default()
                    .with_jitter()
                    .without_max_times(),
            )
            .when(|e|
                // Retry only problems with doing the query itself
                 matches!(e, IdResolveError::PkarrResolve { .. }))
            .notify(|e, _| debug!(target: LOG_TARGET, err = %e.fmt_compact(), "Could not determine the state of published id"))
            .await
    }

    pub async fn post(&self, body: String) -> PostResult<()> {
        pub(crate) const ACTIVE_RESERVATION_TIMEOUT: Duration = Duration::from_secs(120);
        let mut known_head = None;
        let mut active_reservation: Option<(CompactTicket, Instant)> = None;
        let mut signed_event: Option<SignedEvent> = None;

        'try_connect_to_active: loop {
            let published_id_data = self.check_published_id_state().await;

            match published_id_data {
                Ok(published_id_data) => {
                    known_head = published_id_data.published.head.or(known_head);
                    let Some(ticket) = published_id_data.published.ticket else {
                        debug!(target: LOG_TARGET, "Not ticket to join this instance");
                        break 'try_connect_to_active;
                    };

                    if let Some((active_ticket, start)) = active_reservation.as_ref() {
                        if active_ticket == &ticket {
                            if ACTIVE_RESERVATION_TIMEOUT < start.elapsed() {
                                debug!(target: LOG_TARGET, "Reservation stale");
                                break 'try_connect_to_active;
                            }
                        } else {
                            active_reservation = Some((ticket.clone(), Instant::now()));
                        }
                    } else {
                        active_reservation = Some((ticket.clone(), Instant::now()));
                    }

                    let Ok(conn) = self.connect_ticket(ticket).await.inspect_err(|err| {
                        debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Failed to connect to active instance");
                    }) else {
                        continue;
                    };

                    signed_event = Some(signed_event.unwrap_or_else(|| {
                        Event::builder()
                            .author(self.id)
                            .kind(EventKindKnown::SocialPost)
                            .content(body.as_bytes().to_owned().into())
                            .build()
                            .signed_by(self.id_secret)
                    }));

                    let signed_event = signed_event.expect("Must be set by now");
                    match conn
                        .make_rpc_with_extra_data_send(&FeedEventRequest(signed_event), |send| {
                            let body = body.clone();
                            Box::pin(async move {
                                Connection::write_bao_content(
                                    send,
                                    body.as_bytes(),
                                    signed_event.event.content_hash,
                                )
                                .await?;
                                Ok(())
                            })
                        })
                        .await
                    {
                        Ok(_) => {
                            debug!(target: LOG_TARGET, "Published");
                            return Ok(());
                        }
                        Err(RpcError::Failed {
                            return_code: FeedEventResponse::RETURN_CODE_ALREADY_HAVE,
                        }) => {
                            debug!(target: LOG_TARGET, "Already published");
                            return Ok(());
                        }
                        Err(err) => {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Could not upload to active instance");
                        }
                    }
                }
                Err(_) => todo!(),
            }
        }

        Ok(())
    }

    pub fn self_followees_list_subscribe(&self) -> Option<watch::Receiver<Vec<RostraId>>> {
        self.storage
            .as_ref()
            .map(|storage| storage.self_followees_list_subscribe())
    }

    pub fn check_for_updates_tx_subscribe(&self) -> watch::Receiver<()> {
        self.check_for_updates_tx.subscribe()
    }

    pub fn storage_opt(&self) -> Option<Arc<Storage>> {
        self.storage.clone()
    }

    pub fn storage(&self) -> ClientStorageResult<Arc<Storage>> {
        self.storage.clone().context(ClientStorageSnafu)
    }
}
