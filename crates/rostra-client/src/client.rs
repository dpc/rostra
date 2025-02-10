use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ops;
use std::option::Option;
use std::path::Path;
use std::str::FromStr as _;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Weak};
use std::time::Duration;

use backon::Retryable as _;
use iroh::discovery::dns::DnsDiscovery;
use iroh::discovery::pkarr::PkarrPublisher;
use iroh::discovery::ConcurrentDiscovery;
use iroh::NodeAddr;
use itertools::Itertools as _;
use rostra_client_db::{Database, DbResult, IdsFolloweesRecord, IdsFollowersRecord};
use rostra_core::event::{
    content_kind, Event, EventExt as _, EventKind, IrohNodeId, PersonaId, SignedEvent,
    VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};
use rostra_p2p::connection::{Connection, FeedEventRequest, FeedEventResponse, PingRequest};
use rostra_p2p::RpcError;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::FmtCompact as _;
use rostra_util_fmt::AsFmtOption as _;
use snafu::{ensure, Location, OptionExt as _, ResultExt as _, Snafu};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};
use url::Url;

use super::{get_rrecord_typed, RRECORD_HEAD_KEY, RRECORD_P2P_KEY};
use crate::error::{
    ActivateResult, ConnectIrohSnafu, ConnectResult, IdResolveError, IdResolveResult,
    IdSecretReadResult, InitIrohClientSnafu, InitPkarrClientSnafu, InitResult, InvalidIdSnafu,
    IoSnafu, IrohResult, MissingTicketSnafu, ParsingSnafu, PkarrResolveSnafu, PostResult,
    RRecordSnafu, ResolveSnafu, SecretMismatchSnafu,
};
use crate::id::{CompactTicket, IdPublishedData, IdResolvedData};
use crate::task::id_publisher::PkarrIdPublisher;
use crate::task::missing_event_fetcher::MissingEventFetcher;
use crate::task::request_handler::RequestHandler;
use crate::LOG_TARGET;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub struct ClientRefError {
    #[snafu(implicit)]
    location: Location,
}

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
    pub fn client_ref(&self) -> ClientRefResult<ClientRef<'_>> {
        let client = self.0.upgrade().context(ClientRefSnafu)?;
        Ok(ClientRef {
            client,
            r: PhantomData,
        })
    }

    pub fn db(&self) -> ClientRefResult<Arc<Database>> {
        let client = self.0.upgrade().context(ClientRefSnafu)?;

        Ok(client.db().clone())
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

impl ops::Deref for ClientRef<'_> {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    pub(crate) pkarr_client: Arc<pkarr::Client>,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    pub(crate) id: RostraId,

    db: Arc<Database>,

    /// Our iroh-net endpoint
    ///
    /// Each time new random-seed generated one, (optionally) published via
    /// Pkarr under main `RostraId` identity.
    pub(crate) endpoint: iroh::Endpoint,

    /// A watch-channel that can be used to notify some tasks manually to check
    /// for updates again
    check_for_updates_tx: watch::Sender<()>,

    active: AtomicBool,
}

#[bon::bon]
impl Client {
    #[builder(finish_fn(name = "build"))]
    pub async fn new(
        #[builder(start_fn)] id: RostraId,
        #[builder(default = true)] start_request_handler: bool,
        db: Option<Database>,
    ) -> InitResult<Arc<Self>> {
        debug!(target: LOG_TARGET, id = %id, "Starting Rostra client");
        let is_mode_full = db.is_some();

        trace!(target: LOG_TARGET, id = %id, "Creating Pkarr client");
        let pkarr_client = pkarr::Client::builder()
            .relays(vec![
                Url::parse("https://dns.iroh.link/pkarr").expect("Can't fail")
            ])
            .build()
            .context(InitPkarrClientSnafu)?
            .into();

        trace!(target: LOG_TARGET, id = %id, "Creating Iroh endpoint");
        let endpoint = Self::make_iroh_endpoint(db.as_ref().map(|s| s.iroh_secret())).await?;
        let (check_for_updates_tx, _) = watch::channel(());

        let db = if let Some(db) = db {
            db
        } else {
            debug!(target: LOG_TARGET, id = %id, "Creating temporary in-memory database");
            Database::new_in_memory(id).await?
        }
        .into();
        trace!(target: LOG_TARGET, id = %id, "Creating client");
        let client = Arc::new_cyclic(|client| Self {
            handle: client.clone().into(),
            endpoint,
            pkarr_client,
            db,
            id,
            check_for_updates_tx,
            active: AtomicBool::new(false),
        });

        trace!(target: LOG_TARGET, id = %id, "Starting client tasks");
        if start_request_handler {
            client.start_request_handler();
        }

        if is_mode_full {
            client.start_followee_checker();
            client.start_followee_head_checker();
            client.start_head_update_broadcaster();
            client.start_missing_event_fetcher();
        }

        trace!(target: LOG_TARGET, %id, "Client complete");
        Ok(client)
    }
}

#[bon::bon]
impl Client {
    pub fn rostra_id(&self) -> RostraId {
        self.id
    }

    pub async fn unlock_active(&self, id_secret: RostraIdSecretKey) -> ActivateResult<()> {
        ensure!(self.id == id_secret.id(), SecretMismatchSnafu);

        if !self.active.swap(true, SeqCst) {
            self.start_pkarr_id_publisher(id_secret);
        }

        let db = &self.db;

        let our_endpoint = IrohNodeId::from_bytes(*self.endpoint.node_id().as_bytes());
        let endpoints = db.get_id_endpoints(self.rostra_id()).await;

        if let Some((_existing_id, _existing_record)) = endpoints
            .iter()
            .find(|((_ts, endpoint), _)| endpoint == &our_endpoint)
        {
            debug!(target: LOG_TARGET, "Existing node announcement found");
        } else {
            if let Err(err) = self.publish_node_announcement(id_secret).await {
                warn!(target: LOG_TARGET, err = %err.fmt_compact(), "Could not publish node announcement");
            } else {
                info!(target: LOG_TARGET, "Published node announcement");
            }
        }

        Ok(())
    }

    pub async fn publish_node_announcement(&self, id_secret: RostraIdSecretKey) -> PostResult<()> {
        self.publish_event(
            id_secret,
            content_kind::NodeAnnouncement::Iroh {
                addr: IrohNodeId::from_bytes(*self.endpoint.node_id().as_bytes()),
            },
        )
        .call()
        .await?;

        Ok(())
    }

    pub async fn connect(&self, id: RostraId) -> ConnectResult<Connection> {
        // TODO: maintain connection attempt stats and use them to prioritize best
        // endpoints
        for ((_ts, endpoint), _stats) in self.db.get_id_endpoints(id).await.into_iter().rev() {
            let Ok(endpoint) = iroh::NodeId::from_bytes(&endpoint.to_bytes()) else {
                debug!(target: LOG_TARGET, %id, "Invalid iroh id for rostra id found");
                continue;
            };

            if endpoint == self.endpoint.node_id() {
                // If we are trying to connect to our own Id, we want to connect (if possible)
                // with some other node.
                continue;
            }

            let conn = match self.endpoint.connect(endpoint, ROSTRA_P2P_V0_ALPN).await {
                Ok(conn) => Connection::from(conn),
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        %id,
                        %endpoint,
                        err = %format!("{err:#}"),
                        "Failed to connect to a know iroh endpoint"
                    );
                    continue;
                }
            };

            // Make a ping request, just to make sure we can talk
            match conn.make_rpc(&PingRequest(0)).await {
                Ok(_) => return Ok(conn),
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        %id,
                        %endpoint,
                        err = %format!("{err:#}"),
                        "Failed to ping a know iroh endpoint"
                    );
                    continue;
                }
            }
        }

        let ticket = self.resolve_id_ticket(id).await.context(ResolveSnafu)?;

        let node_addr = NodeAddr::from(ticket);
        debug!(target: LOG_TARGET, iroh_id = %node_addr.node_id, id = %id.to_short(), "Connecting");
        Ok(self
            .endpoint
            .connect(node_addr, ROSTRA_P2P_V0_ALPN)
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

    pub(crate) async fn make_iroh_endpoint(
        iroh_secret: impl Into<Option<iroh::SecretKey>>,
    ) -> InitResult<iroh::Endpoint> {
        use iroh::{Endpoint, SecretKey};
        let secret_key = iroh_secret
            .into()
            .unwrap_or_else(|| SecretKey::generate(&mut rand::thread_rng()));

        let discovery = ConcurrentDiscovery::from_services(vec![
            Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
            Box::new(DnsDiscovery::n0_dns()),
        ]);

        // We rely entirely on tickets published by our own publisher
        // for every RostraId via Pkarr, so we don't need discovery
        let ep = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .discovery(Box::new(discovery))
            .bind()
            .await
            .context(InitIrohClientSnafu)?;
        debug!(target: LOG_TARGET, iroh_id = %ep.node_id(), "Created Iroh endpoint");
        Ok(ep)
    }

    pub(crate) fn start_pkarr_id_publisher(&self, secret_id: RostraIdSecretKey) {
        tokio::spawn(PkarrIdPublisher::new(self, secret_id).run());
    }

    pub(crate) fn start_request_handler(&self) {
        tokio::spawn(RequestHandler::new(self, self.endpoint.clone()).run());
    }

    pub(crate) fn start_followee_checker(&self) {
        tokio::spawn(crate::task::followee_checker::FolloweeChecker::new(self).run());
    }
    pub(crate) fn start_followee_head_checker(&self) {
        tokio::spawn(crate::task::followee_head_checker::FolloweeHeadChecker::new(self).run());
    }
    pub(crate) fn start_head_update_broadcaster(&self) {
        tokio::spawn(crate::task::head_update_broadcaster::HeadUpdateBroadcaster::new(self).run());
    }
    pub(crate) fn start_missing_event_fetcher(&self) {
        tokio::spawn(MissingEventFetcher::new(self).run());
    }

    pub(crate) async fn iroh_address(&self) -> IrohResult<NodeAddr> {
        pub(crate) fn sanitize_node_addr(node_addr: NodeAddr) -> NodeAddr {
            pub(crate) fn is_ipv4_cgnat(ip: Ipv4Addr) -> bool {
                matches!(ip.octets(), [100, b, ..] if (64..128).contains(&b))
            }
            let direct_addresses = node_addr
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
            NodeAddr {
                direct_addresses,
                ..node_addr
            }
        }

        self.endpoint.node_addr().await.map(sanitize_node_addr)
    }

    pub fn self_head_subscribe(&self) -> watch::Receiver<Option<ShortEventId>> {
        self.db.self_head_subscribe()
    }

    pub fn new_content_subscribe(&self) -> broadcast::Receiver<VerifiedEventContent> {
        self.db.new_content_subscribe()
    }

    pub fn new_posts_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::SocialPost)> {
        self.db.new_posts_subscribe()
    }

    pub fn check_for_updates_tx_subscribe(&self) -> watch::Receiver<()> {
        self.check_for_updates_tx.subscribe()
    }

    pub fn ids_with_missing_events_subscribe(
        &self,
        capacity: usize,
    ) -> dedup_chan::Receiver<RostraId> {
        self.db.ids_with_missing_events_subscribe(capacity)
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub async fn events_head(&self) -> Option<ShortEventId> {
        self.db.get_self_current_head().await
    }

    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    pub async fn resolve_id_data(&self, id: RostraId) -> IdResolveResult<IdResolvedData> {
        let public_key = pkarr::PublicKey::try_from(id).context(InvalidIdSnafu)?;
        let domain = public_key.to_string();
        let packet = self
            .pkarr_client
            .resolve(&public_key)
            .await
            .context(PkarrResolveSnafu)?;

        let timestamp = packet.timestamp();
        let ticket = get_rrecord_typed(&packet, &domain, RRECORD_P2P_KEY).context(RRecordSnafu)?;
        let head = get_rrecord_typed(&packet, &domain, RRECORD_HEAD_KEY).context(RRecordSnafu)?;

        debug!(
            target: LOG_TARGET,
            %id,
            ticket = %ticket.fmt_option(),
            head=%head.fmt_option(),
            "Resolved Id"
        );

        Ok(IdResolvedData {
            published: IdPublishedData { ticket, head },
            timestamp: timestamp.as_u64(),
        })
    }

    pub async fn resolve_id_ticket(&self, id: RostraId) -> IdResolveResult<CompactTicket> {
        self.resolve_id_data(id)
            .await?
            .published
            .ticket
            .context(MissingTicketSnafu)
    }

    pub(crate) fn pkarr_client(&self) -> Arc<pkarr::Client> {
        self.pkarr_client.clone()
    }

    pub(crate) async fn does_have_event(&self, _event_id: rostra_core::EventId) -> bool {
        // TODO: check
        false
    }

    pub async fn store_event_with_content(
        &self,
        _event_id: impl Into<ShortEventId>,
        content: &VerifiedEventContent,
    ) {
        self.db.process_event_with_content(content).await;
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

    #[builder]
    pub async fn publish_event<C>(
        &self,
        #[builder(start_fn)] id_secret: RostraIdSecretKey,
        #[builder(start_fn)] content: C,
        replace: Option<ShortEventId>,
    ) -> PostResult<VerifiedEvent>
    where
        C: content_kind::EventContentKind,
    {
        let current_head = self.db.get_self_current_head().await;
        let aux_event = if replace.is_some() {
            None
        } else {
            self.db.get_self_random_eventid().await
        };

        let content = content.serialize_cbor()?;

        let signed_event = Event::builder()
            .author(self.id)
            .kind(C::KIND)
            .content(&content)
            .maybe_parent_prev(current_head)
            .maybe_parent_aux(aux_event)
            .maybe_delete(replace)
            .build()
            .signed_by(id_secret);

        let verified_event = VerifiedEvent::verify_signed(self.id, signed_event)
            .expect("Can't fail to verify self-created event");
        let verified_event_content =
            rostra_core::event::VerifiedEventContent::verify(verified_event, content)
                .expect("Can't fail to verify self-created content");
        let _ = self
            .db
            .process_event_with_content(&verified_event_content)
            .await;

        Ok(verified_event)
    }

    pub async fn social_post(
        &self,
        id_secret: RostraIdSecretKey,
        body: String,
        reply_to: Option<ExternalEventId>,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::SocialPost {
                djot_content: body,
                persona: PersonaId(0),
                reply_to,
            },
        )
        .call()
        .await
    }
    pub async fn post_social_profile_update(
        &self,
        id_secret: RostraIdSecretKey,
        display_name: String,
        bio: String,
        avatar: Option<(String, Vec<u8>)>,
    ) -> PostResult<VerifiedEvent> {
        let existing = self
            .db
            .get_social_profile(self.rostra_id())
            .await
            .map(|r| r.event_id);
        self.publish_event(
            id_secret,
            content_kind::SocialProfileUpdate {
                display_name,
                bio,
                avatar,
            },
        )
        .maybe_replace(existing)
        .call()
        .await
    }

    pub async fn follow(
        &self,
        id_secret: RostraIdSecretKey,
        followee: RostraId,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::Follow {
                followee,
                persona: PersonaId(0),
            },
        )
        .call()
        .await
    }

    pub async fn publish_omni_tbd(
        &self,
        id_secret: RostraIdSecretKey,
        body: String,
    ) -> PostResult<()> {
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
                            .kind(EventKind::SOCIAL_POST)
                            .content(&body.as_bytes().to_owned().into())
                            .build()
                            .signed_by(id_secret)
                    }));

                    let signed_event = signed_event.expect("Must be set by now");
                    match conn
                        .make_rpc_with_extra_data_send(&FeedEventRequest(signed_event), |send| {
                            let body = body.clone();
                            Box::pin(async move {
                                Connection::write_bao_content(
                                    send,
                                    body.as_bytes(),
                                    signed_event.content_hash(),
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

    pub fn self_followees_subscribe(
        &self,
    ) -> watch::Receiver<HashMap<RostraId, IdsFolloweesRecord>> {
        self.db.self_followees_subscribe()
    }

    pub fn self_followers_subscribe(
        &self,
    ) -> watch::Receiver<HashMap<RostraId, IdsFollowersRecord>> {
        self.db.self_followers_subscribe()
    }
}
