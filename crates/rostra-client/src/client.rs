use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::Ipv4Addr;
use std::ops;
use std::option::Option;
use std::path::Path;
use std::str::FromStr as _;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Weak};
use std::time::Duration;

use backon::Retryable as _;
use iroh::address_lookup::dns::DnsAddressLookup;
use iroh::address_lookup::pkarr::PkarrPublisher;
use iroh_base::EndpointAddr;
use rostra_client_db::{Database, DbResult, IdsFolloweesRecord, IdsFollowersRecord, WotData};
use rostra_core::event::{
    Event, EventContentRaw, IrohNodeId, PersonaId, PersonaSelector, SignedEvent, SocialPost,
    VerifiedEvent, VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_p2p::RpcError;
use rostra_p2p::connection::{Connection, FeedEventResponse};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::{FmtCompact as _, WhateverResult};
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu, ensure};
use tokio::sync::{RwLock, broadcast, watch};
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

use crate::LOG_TARGET;
use crate::error::{
    ActivateResult, ActivateSnafu, ConnectResult, IdResolveError, IdResolveResult,
    IdSecretReadResult, InitIrohClientSnafu, InitPkarrClientSnafu, InitResult, IoSnafu,
    ParsingSnafu, PostResult, SecretMismatchSnafu,
};
use crate::id::{CompactTicket, IdResolvedData};
use crate::task::head_merger::HeadMerger;
use crate::task::missing_event_content_fetcher::MissingEventContentFetcher;
use crate::task::missing_event_fetcher::MissingEventFetcher;
use crate::task::pkarr_id_publisher::PkarrIdPublisher;
use crate::task::request_handler::RequestHandler;

/// Per-identity P2P connection state for debugging.
///
/// Tracks connection attempts, successes, failures, and head check results.
/// This is in-memory only and populates over time as connections are made.
#[derive(Debug, Clone, Default)]
pub struct IdP2PState {
    /// Last time we attempted to connect to this ID
    pub last_attempt: Option<Timestamp>,
    /// Last successful connection time
    pub last_success: Option<Timestamp>,
    /// Last failed connection time
    pub last_failure: Option<Timestamp>,
    /// Last head resolved from pkarr TXT record
    pub last_pkarr_head: Option<ShortEventId>,
    /// Timestamp of last pkarr resolution
    pub last_pkarr_resolve: Option<Timestamp>,
    /// Last head obtained from iroh connection
    pub last_checked_head: Option<ShortEventId>,
    /// Timestamp of last head check via iroh
    pub last_head_check: Option<Timestamp>,
}

/// Per-node (Iroh endpoint) connection state for debugging.
///
/// Tracks connection attempts, successes, and failures per node.
#[derive(Debug, Clone, Default)]
pub struct NodeP2PState {
    /// Last time we attempted to connect to this node
    pub last_attempt: Option<Timestamp>,
    /// Last successful connection time
    pub last_success: Option<Timestamp>,
    /// Last failed connection time
    pub last_failure: Option<Timestamp>,
    /// Source of how we learned about this node
    pub source: NodeSource,
    /// The RostraId this node is associated with (if known)
    pub rostra_id: Option<RostraId>,
    /// Number of consecutive connection failures
    pub consecutive_failures: u32,
    /// Time until which we should not attempt to connect (backoff)
    pub backoff_until: Option<Instant>,
}

/// Maximum backoff duration for failed connection attempts (10 minutes)
pub const MAX_BACKOFF_DURATION: Duration = Duration::from_secs(10 * 60);

/// Initial backoff duration for failed connection attempts (1 second)
pub const INITIAL_BACKOFF_DURATION: Duration = Duration::from_secs(1);

impl NodeP2PState {
    /// Calculate the backoff duration based on consecutive failures.
    ///
    /// Uses exponential backoff: 1s, 2s, 4s, 8s, ... capped at 10 minutes.
    pub fn calculate_backoff_duration(&self) -> Duration {
        if self.consecutive_failures == 0 {
            return Duration::ZERO;
        }
        let shift = self.consecutive_failures.saturating_sub(1).min(63);
        let multiplier = 1u64 << shift;
        let backoff_secs = INITIAL_BACKOFF_DURATION
            .as_secs()
            .saturating_mul(multiplier);
        Duration::from_secs(backoff_secs).min(MAX_BACKOFF_DURATION)
    }

    /// Check if we should skip connecting due to backoff.
    pub fn is_in_backoff(&self) -> bool {
        self.backoff_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }

    /// Record a successful connection, resetting backoff state.
    pub fn record_success(&mut self, now: Timestamp) {
        self.last_success = Some(now);
        self.consecutive_failures = 0;
        self.backoff_until = None;
    }

    /// Record a failed connection, updating backoff state.
    pub fn record_failure(&mut self, now: Timestamp) {
        self.last_failure = Some(now);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let backoff_duration = self.calculate_backoff_duration();
        self.backoff_until = Some(Instant::now() + backoff_duration);
    }
}

/// How we learned about a node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NodeSource {
    /// From a NodeAnnouncement event stored in the database
    #[default]
    NodeAnnouncement,
    /// From pkarr DNS resolution
    Pkarr,
}

/// In-memory P2P state for all known identities.
///
/// Used by the P2P Explorer UI to display connection and resolution status.
#[derive(Debug, Default)]
pub struct P2PState {
    ids: RwLock<HashMap<RostraId, IdP2PState>>,
    nodes: RwLock<HashMap<IrohNodeId, NodeP2PState>>,
}

impl P2PState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the P2P state for a specific identity.
    pub async fn get(&self, id: RostraId) -> IdP2PState {
        self.ids.read().await.get(&id).cloned().unwrap_or_default()
    }

    /// Get P2P state for all known identities.
    pub async fn get_all(&self) -> HashMap<RostraId, IdP2PState> {
        self.ids.read().await.clone()
    }

    /// Update the P2P state for an identity.
    pub async fn update(&self, id: RostraId, f: impl FnOnce(&mut IdP2PState)) {
        let mut ids = self.ids.write().await;
        let state = ids.entry(id).or_default();
        f(state);
    }

    /// Get the P2P state for a specific node.
    pub async fn get_node(&self, node_id: IrohNodeId) -> NodeP2PState {
        self.nodes
            .read()
            .await
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get P2P state for all known nodes.
    pub async fn get_all_nodes(&self) -> HashMap<IrohNodeId, NodeP2PState> {
        self.nodes.read().await.clone()
    }

    /// Update the P2P state for a node.
    pub async fn update_node(&self, node_id: IrohNodeId, f: impl FnOnce(&mut NodeP2PState)) {
        let mut nodes = self.nodes.write().await;
        let state = nodes.entry(node_id).or_default();
        f(state);
    }

    /// Check if a node is currently in backoff.
    pub async fn is_node_in_backoff(&self, node_id: IrohNodeId) -> bool {
        self.nodes
            .read()
            .await
            .get(&node_id)
            .map(|s| s.is_in_backoff())
            .unwrap_or(false)
    }

    /// Get the remaining backoff duration for a node, if any.
    pub async fn get_node_backoff_remaining(&self, node_id: IrohNodeId) -> Option<Duration> {
        let nodes = self.nodes.read().await;
        let state = nodes.get(&node_id)?;
        let until = state.backoff_until?;
        let now = Instant::now();
        if now < until { Some(until - now) } else { None }
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub struct ClientRefError {
    #[snafu(implicit)]
    location: Location,
}

pub type ClientRefResult<T> = Result<T, ClientRefError>;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
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

impl ClientRef<'_> {
    /// Connect to a peer using the shared connection cache
    ///
    /// Returns a cached connection if available, otherwise creates a new one.
    /// This is more efficient than `connect_uncached` when making repeated
    /// connections to the same peer.
    pub async fn connect_cached(&self, id: RostraId) -> ConnectResult<Connection> {
        self.networking.connect_cached(id).await
    }
}

pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    pub(crate) id: RostraId,

    pub(crate) db: Arc<Database>,

    active: AtomicBool,

    /// Networking layer (endpoint, pkarr, p2p_state, connection cache)
    pub(crate) networking: Arc<crate::net::ClientNetworking>,
}

#[bon::bon]
impl Client {
    #[builder(finish_fn(name = "build"))]
    pub async fn new(
        #[builder(start_fn)] id: RostraId,
        #[builder(default = true)] start_request_handler: bool,
        /// When false, skips spawning background tasks (head checker, event
        /// fetchers, etc.) even when a DB is provided. Useful for tests.
        #[builder(default = true)]
        start_background_tasks: bool,
        db: Option<Database>,
        secret: Option<RostraIdSecretKey>,
        /// When true, allows direct IP connections (exposes IP address).
        /// When false (default), uses relay-only mode for privacy.
        #[builder(default = false)]
        public_mode: bool,
        /// Pre-built iroh endpoint. If provided, uses this instead of
        /// creating a new one. Useful for tests that need custom endpoint
        /// configuration.
        iroh_endpoint: Option<iroh::Endpoint>,
    ) -> InitResult<Arc<Self>> {
        debug!(target: LOG_TARGET, id = %id, "Starting Rostra client");
        let client_start = Instant::now();
        let is_mode_full = db.is_some();

        trace!(target: LOG_TARGET, id = %id, "Creating Pkarr client");
        let pkarr_client = pkarr::Client::builder()
            .relays(&["https://dns.iroh.link/pkarr"])
            .expect("Can't fail")
            .build()
            .context(InitPkarrClientSnafu)?
            .into();
        debug!(target: LOG_TARGET, id = %id, elapsed_ms = %client_start.elapsed().as_millis(), "Pkarr client created");

        let endpoint = if let Some(ep) = iroh_endpoint {
            ep
        } else {
            trace!(target: LOG_TARGET, id = %id, "Creating Iroh endpoint");
            let ep =
                Self::make_iroh_endpoint(db.as_ref().map(|s| s.iroh_secret()), public_mode).await?;
            debug!(target: LOG_TARGET, id = %id, elapsed_ms = %client_start.elapsed().as_millis(), "Iroh endpoint created");
            ep
        };
        let db: Arc<Database> = match db {
            Some(db) => db,
            _ => {
                debug!(target: LOG_TARGET, id = %id, "Creating temporary in-memory database");
                Database::new_in_memory(id).await?
            }
        }
        .into();
        trace!(target: LOG_TARGET, id = %id, "Creating client");
        let networking = Arc::new(crate::net::ClientNetworking::new(
            endpoint,
            pkarr_client,
            db.clone() as Arc<dyn crate::net::IdEndpointLookup>,
        ));
        let client = Arc::new_cyclic(|client| Self {
            handle: client.clone().into(),
            networking,
            db,
            id,
            active: AtomicBool::new(false),
        });

        trace!(target: LOG_TARGET, id = %id, "Starting client tasks");
        if start_request_handler {
            client.start_request_handler();
        }

        if is_mode_full && start_background_tasks {
            client.start_head_update_broadcaster();
            client.start_missing_event_fetcher();
            client.start_missing_event_content_fetcher();
            client.start_new_head_fetcher();
            client.start_poll_follower_head_updates();
            client.start_poll_followee_head_updates();
        }

        if let Some(secret) = secret {
            client.unlock_active(secret).await.context(ActivateSnafu)?;
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
        let unlock_start = Instant::now();
        ensure!(self.id == id_secret.id(), SecretMismatchSnafu);

        if !self.active.swap(true, SeqCst) {
            self.start_pkarr_id_publisher(id_secret);
            self.start_head_merger(id_secret);
        }

        let db = &self.db;

        let our_endpoint = IrohNodeId::from_bytes(*self.networking.endpoint.id().as_bytes());
        let endpoints = db.get_id_endpoints(self.rostra_id()).await;
        debug!(target: LOG_TARGET, elapsed_ms = %unlock_start.elapsed().as_millis(), "Fetched id endpoints");

        if let Some((_existing_id, _existing_record)) = endpoints
            .iter()
            .find(|((_ts, endpoint), _)| endpoint == &our_endpoint)
        {
            debug!(target: LOG_TARGET, "Existing node announcement found");
        } else {
            match self.publish_node_announcement(id_secret).await {
                Err(err) => {
                    warn!(target: LOG_TARGET, err = %err.fmt_compact(), "Could not publish node announcement");
                }
                _ => {
                    info!(target: LOG_TARGET, "Published node announcement");
                }
            }
            debug!(target: LOG_TARGET, elapsed_ms = %unlock_start.elapsed().as_millis(), "Node announcement published");
        }

        debug!(target: LOG_TARGET, elapsed_ms = %unlock_start.elapsed().as_millis(), "unlock_active complete");
        Ok(())
    }

    pub async fn publish_node_announcement(&self, id_secret: RostraIdSecretKey) -> PostResult<()> {
        self.publish_event(
            id_secret,
            content_kind::NodeAnnouncement::Iroh {
                addr: IrohNodeId::from_bytes(*self.networking.endpoint.id().as_bytes()),
            },
        )
        .call()
        .await?;

        Ok(())
    }

    pub(crate) async fn make_iroh_endpoint(
        iroh_secret: impl Into<Option<iroh::SecretKey>>,
        public_mode: bool,
    ) -> InitResult<iroh::Endpoint> {
        use iroh::{Endpoint, SecretKey};
        let secret_key = iroh_secret
            .into()
            .unwrap_or_else(|| SecretKey::generate(&mut rand::rng()));

        // We rely entirely on tickets published by our own publisher
        // for every RostraId via Pkarr, so we don't need discovery
        // Address lookup is used for publishing our address and resolving others
        let mut builder = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .address_lookup(PkarrPublisher::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns());

        // By default, use relay-only mode for privacy (no direct IP connections).
        // In public mode, allow direct IP connections (useful for hosted nodes).
        if !public_mode {
            builder = builder.clear_ip_transports();
        }

        let ep = builder.bind().await.context(InitIrohClientSnafu)?;
        let iroh_id_z32 = z32::encode(ep.id().as_bytes());
        debug!(target: LOG_TARGET, iroh_id = %ep.id(), %iroh_id_z32, public_mode, "Created Iroh endpoint");
        Ok(ep)
    }

    pub(crate) fn start_pkarr_id_publisher(&self, secret_id: RostraIdSecretKey) {
        tokio::spawn(PkarrIdPublisher::new(self, secret_id).run());
    }

    pub(crate) fn start_head_merger(&self, secret_id: RostraIdSecretKey) {
        tokio::spawn(HeadMerger::new(self, secret_id).run());
    }

    pub(crate) fn start_request_handler(&self) {
        tokio::spawn(RequestHandler::new(self, self.networking.endpoint.clone()).run());
    }

    pub(crate) fn start_head_update_broadcaster(&self) {
        tokio::spawn(crate::task::head_update_broadcaster::HeadUpdateBroadcaster::new(self).run());
    }
    pub(crate) fn start_missing_event_fetcher(&self) {
        tokio::spawn(MissingEventFetcher::new(self).run());
    }
    pub(crate) fn start_missing_event_content_fetcher(&self) {
        tokio::spawn(MissingEventContentFetcher::new(self).run());
    }

    pub(crate) fn start_new_head_fetcher(&self) {
        tokio::spawn(crate::task::new_head_fetcher::NewHeadFetcher::new(self).run());
    }

    pub(crate) fn start_poll_follower_head_updates(&self) {
        tokio::spawn(
            crate::task::poll_follower_head_updates::PollFollowerHeadUpdates::new(self).run(),
        );
    }

    pub(crate) fn start_poll_followee_head_updates(&self) {
        tokio::spawn(
            crate::task::poll_followee_head_updates::PollFolloweeHeadUpdates::new(self).run(),
        );
    }

    pub(crate) async fn iroh_address(&self) -> WhateverResult<EndpointAddr> {
        pub(crate) fn sanitize_endpoint_addr(endpoint_addr: EndpointAddr) -> EndpointAddr {
            use iroh_base::TransportAddr;
            pub(crate) fn is_ipv4_cgnat(ip: Ipv4Addr) -> bool {
                matches!(ip.octets(), [100, b, ..] if (64..128).contains(&b))
            }
            let filtered_addrs = endpoint_addr
                .addrs
                .into_iter()
                .filter(|addr| match addr {
                    TransportAddr::Ip(socket_addr) => match socket_addr {
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
                    },
                    TransportAddr::Relay(_) => true, // Keep relay addresses
                    _ => true, // Keep any future address types
                })
                .collect();
            EndpointAddr {
                id: endpoint_addr.id,
                addrs: filtered_addrs,
            }
        }

        Ok(sanitize_endpoint_addr(self.networking.endpoint.addr()))
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

    pub fn new_shoutbox_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::Shoutbox)> {
        self.db.new_shoutbox_subscribe()
    }

    pub fn new_heads_subscribe(&self) -> broadcast::Receiver<(RostraId, ShortEventId)> {
        self.db.new_heads_subscribe()
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

    /// Access in-memory P2P connection state for debugging.
    pub fn p2p_state(&self) -> &P2PState {
        self.networking.p2p_state()
    }

    /// Access the shared connection cache.
    pub fn connection_cache(&self) -> &crate::connection_cache::ConnectionCache {
        self.networking.connection_cache()
    }

    /// Access the networking layer.
    pub fn networking(&self) -> &Arc<crate::net::ClientNetworking> {
        &self.networking
    }

    /// Returns our local Iroh node ID.
    pub fn local_iroh_id(&self) -> IrohNodeId {
        IrohNodeId::from_bytes(*self.networking.endpoint.id().as_bytes())
    }

    pub async fn resolve_id_data(&self, id: RostraId) -> IdResolveResult<IdResolvedData> {
        self.networking.resolve_id_data(id).await
    }

    pub async fn resolve_id_ticket(&self, id: RostraId) -> IdResolveResult<CompactTicket> {
        self.networking.resolve_id_ticket(id).await
    }

    pub async fn connect_uncached(&self, id: RostraId) -> ConnectResult<Connection> {
        self.networking.connect_uncached(id).await
    }

    pub async fn connect_by_pkarr_resolution(&self, id: RostraId) -> ConnectResult<Connection> {
        self.networking.connect_by_pkarr_resolution(id).await
    }

    pub async fn connect_ticket(&self, ticket: CompactTicket) -> ConnectResult<Connection> {
        self.networking.connect_ticket(ticket).await
    }

    pub(crate) fn pkarr_client(&self) -> Arc<pkarr::Client> {
        self.networking.pkarr_client.clone()
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
                 matches!(e, IdResolveError::PkarrResolve))
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

        let (event, content) = Event::builder(&content)
            .author(self.id)
            .maybe_parent_prev(current_head)
            .maybe_parent_aux(aux_event)
            .maybe_delete(replace)
            .build()?;

        let signed_event = event.signed_by(id_secret);

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

    pub async fn post_shoutbox(
        &self,
        id_secret: RostraIdSecretKey,
        body: String,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(id_secret, content_kind::Shoutbox { djot_content: body })
            .call()
            .await
    }

    pub async fn social_post(
        &self,
        id_secret: RostraIdSecretKey,
        body: String,
        reply_to: Option<ExternalEventId>,
        persona: PersonaId,
    ) -> PostResult<VerifiedEvent> {
        let (content, reaction) =
            if let Some(reaction) = content_kind::SocialPost::is_reaction(&reply_to, &body) {
                (None, Some(reaction.to_owned()))
            } else {
                (Some(body), None)
            };
        self.publish_event(
            id_secret,
            content_kind::SocialPost {
                djot_content: content,
                persona,
                reply_to,
                reaction,
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
        followee_id: RostraId,
        selector: PersonaSelector,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::Follow {
                followee: followee_id,
                selector: Some(selector),
                persona: None,
            },
        )
        .call()
        .await
    }

    pub async fn unfollow(
        &self,
        id_secret: RostraIdSecretKey,
        followee: RostraId,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::Follow {
                followee,
                persona: None,
                selector: None,
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
        let mut event_and_content: Option<(SignedEvent, EventContentRaw)> = None;

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

                    if event_and_content.is_none() {
                        event_and_content = Some({
                            let (event, content) = Event::builder(&SocialPost {
                                djot_content: Some(body.clone()),
                                persona: PersonaId(0),
                                reply_to: None,
                                reaction: None,
                            })
                            .author(self.id)
                            .build()?;

                            (event.signed_by(id_secret), content)
                        });
                    }

                    let (signed_event, raw_content) =
                        event_and_content.as_ref().expect("Must be set by now");
                    match conn.feed_event(*signed_event, raw_content.clone()).await {
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
    ) -> watch::Receiver<Arc<HashMap<RostraId, IdsFolloweesRecord>>> {
        self.db.self_followees_subscribe()
    }

    pub fn self_followers_subscribe(
        &self,
    ) -> watch::Receiver<Arc<HashMap<RostraId, IdsFollowersRecord>>> {
        self.db.self_followers_subscribe()
    }

    pub fn self_wot_subscribe(&self) -> watch::Receiver<Arc<WotData>> {
        self.db.self_wot_subscribe()
    }
}
