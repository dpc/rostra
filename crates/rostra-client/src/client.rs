use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::marker::PhantomData;
use std::net::Ipv4Addr;
use std::ops;
use std::option::Option;
use std::path::Path;
use std::str::FromStr as _;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use backon::Retryable as _;
use iroh_base::EndpointAddr;
use n0_future::task::AbortOnDropHandle;
use rostra_client_db::{
    CurrentState, Database, DbError, DbResult, IdsFolloweesRecord, IdsFollowersRecord, WotData,
};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, IrohNodeId, PersonaTag, PersonasTagsSelector,
    SignedEvent, SocialPost, VerifiedEvent, VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_p2p::RpcError;
use rostra_p2p::connection::{Connection, FeedEventResponse};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::{FmtCompact as _, WhateverResult};
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu, ensure};
use tokio::sync::{RwLock, broadcast};
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

use crate::LOG_TARGET;
use crate::error::{
    ActivateResult, ActivateSnafu, ConnectResult, IdResolveError, IdResolveResult,
    IdSecretReadResult, InitIrohClientSnafu, InitPkarrClientSnafu, InitResult, IoSnafu,
    LocalAnnouncementStorageSnafu, ParsingSnafu, PostResult, SecretMismatchSnafu, StorageSnafu,
    StoreEventError, StoreEventResult,
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
    pub(crate) fn calculate_backoff_duration(&self) -> Duration {
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
    pub(crate) fn record_success(&mut self, now: Timestamp) {
        self.last_success = Some(now);
        self.consecutive_failures = 0;
        self.backoff_until = None;
    }

    /// Record a failed connection, updating backoff state.
    pub(crate) fn record_failure(&mut self, now: Timestamp) {
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

const P2P_IDS_WARN_LIMIT: usize = 10_000;
const P2P_NODES_WARN_LIMIT: usize = 10_000;

impl P2PState {
    pub(crate) fn new() -> Self {
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
    pub(crate) async fn update(&self, id: RostraId, f: impl FnOnce(&mut IdP2PState)) {
        let mut ids = self.ids.write().await;
        let state = ids.entry(id).or_default();
        f(state);
        if ids.len() == P2P_IDS_WARN_LIMIT + 1 {
            warn!(
                target: LOG_TARGET,
                len = ids.len(),
                limit = P2P_IDS_WARN_LIMIT,
                "P2P identity state map is large"
            );
        }
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
    pub(crate) async fn update_node(&self, node_id: IrohNodeId, f: impl FnOnce(&mut NodeP2PState)) {
        let mut nodes = self.nodes.write().await;
        let state = nodes.entry(node_id).or_default();
        f(state);
        if nodes.len() == P2P_NODES_WARN_LIMIT + 1 {
            warn!(
                target: LOG_TARGET,
                len = nodes.len(),
                limit = P2P_NODES_WARN_LIMIT,
                "P2P node state map is large"
            );
        }
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

/// The networked runtime for one Rostra identity.
///
/// Construct clients with [`Client::builder`]. The returned [`Arc<Self>`] owns
/// all request handling and background work, which stops when the final strong
/// reference is dropped.
pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    pub(crate) id: RostraId,

    pub(crate) db: Arc<Database>,

    active: AtomicBool,

    /// Serializes the fallible transition into active/signing mode.
    activation_lock: tokio::sync::Mutex<()>,

    /// Networking layer (endpoint, pkarr, p2p_state, connection cache)
    pub(crate) networking: Arc<crate::net::ClientNetworking>,

    task_handles: Mutex<Vec<AbortOnDropHandle<()>>>,
}

#[bon::bon]
impl Client {
    /// Construct a client for `id`.
    ///
    /// Without `db`, the client uses temporary in-memory storage and does not
    /// start full-client background synchronization tasks. Supplying a durable
    /// `db` enables the supported full-client mode. The request handler starts
    /// by default, background tasks start by default for full clients, and
    /// client-created networking is relay-only unless `public_mode` is
    /// explicitly enabled. A supplied `iroh_endpoint` retains its existing
    /// transport and privacy policy instead.
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
        /// configuration. The caller owns the endpoint's direct-transport and
        /// privacy policy; `public_mode` does not reconfigure it. The caller
        /// must also configure safe QUIC stream counts and receive
        /// windows before binding. Post-handshake reductions cannot
        /// retract transport credit that an endpoint already
        /// advertised.
        iroh_endpoint: Option<iroh::Endpoint>,
        /// Pre-built pkarr client. If provided, uses this instead of
        /// creating a new one. Since the pkarr client is identity-agnostic,
        /// a single instance can be shared across all Rostra clients.
        /// Use [`Client::make_pkarr_client`] to create one.
        pkarr_client: Option<Arc<pkarr::Client>>,
    ) -> InitResult<Arc<Self>> {
        debug!(target: LOG_TARGET, id = %id, "Starting Rostra client");
        let client_start = Instant::now();
        let is_mode_full = db.is_some();

        let pkarr_client = if let Some(pc) = pkarr_client {
            pc
        } else {
            trace!(target: LOG_TARGET, id = %id, "Creating Pkarr client");
            let pc = Self::make_pkarr_client()?;
            debug!(target: LOG_TARGET, id = %id, elapsed_ms = %client_start.elapsed().as_millis(), "Pkarr client created");
            pc
        };

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
            activation_lock: tokio::sync::Mutex::new(()),
            task_handles: Mutex::new(Vec::new()),
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
            client.start_wot_head_sync();
            client.start_news_score_updater();
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
    /// Return the identity served by this client.
    pub fn rostra_id(&self) -> RostraId {
        self.id
    }

    /// Unlock signing operations and start signing-dependent background tasks.
    ///
    /// The secret must belong to [`Client::rostra_id`]. Calling this method
    /// more than once is harmless after the first successful activation.
    pub async fn unlock_active(&self, id_secret: RostraIdSecretKey) -> ActivateResult<()> {
        let unlock_start = Instant::now();
        ensure!(self.id == id_secret.id(), SecretMismatchSnafu);
        let _activation_guard = self.activation_lock.lock().await;
        if self.active.load(SeqCst) {
            return Ok(());
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
            self.finish_activation(id_secret, Ok(()))?;
        } else {
            let announcement_result = self.publish_node_announcement(id_secret).await;
            self.finish_activation(id_secret, announcement_result)?;
            info!(target: LOG_TARGET, "Published node announcement");
            debug!(target: LOG_TARGET, elapsed_ms = %unlock_start.elapsed().as_millis(), "Node announcement published");
        }

        debug!(target: LOG_TARGET, elapsed_ms = %unlock_start.elapsed().as_millis(), "unlock_active complete");
        Ok(())
    }

    fn finish_activation(
        &self,
        id_secret: RostraIdSecretKey,
        announcement_result: PostResult<()>,
    ) -> ActivateResult<()> {
        announcement_result.context(LocalAnnouncementStorageSnafu)?;
        self.active.store(true, SeqCst);
        self.start_pkarr_id_publisher(id_secret);
        self.start_head_merger(id_secret);
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

    /// Create a shared pkarr client.
    ///
    /// The pkarr client is identity-agnostic, so a single instance can
    /// be reused across all Rostra clients via the `pkarr_client`
    /// parameter on [`Client::builder`].
    pub fn make_pkarr_client() -> InitResult<Arc<pkarr::Client>> {
        Ok(Arc::new(
            pkarr::Client::builder()
                .relays(&["https://dns.iroh.link/pkarr"])
                .expect("Can't fail")
                .build()
                .context(InitPkarrClientSnafu)?,
        ))
    }

    pub(crate) async fn make_iroh_endpoint(
        iroh_secret: impl Into<Option<iroh::SecretKey>>,
        public_mode: bool,
    ) -> InitResult<iroh::Endpoint> {
        use iroh::endpoint::{QuicTransportConfig, VarInt, presets};
        use iroh::{Endpoint, SecretKey};
        let secret_key = iroh_secret.into().unwrap_or_else(SecretKey::generate);

        // Use the n0 preset for relay transport and Pkarr/DNS address lookup.
        // Rostra publishes its own tickets through Pkarr for each identity.
        let transport = QuicTransportConfig::builder()
            .max_concurrent_bidi_streams(VarInt::from_u32(32))
            .max_concurrent_uni_streams(VarInt::from_u32(0))
            .stream_receive_window(VarInt::from_u32(64 * 1024))
            .receive_window(VarInt::from_u32(2 * 1024 * 1024))
            .build();
        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .transport_config(transport);

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

    fn spawn_task(&self, future: impl Future<Output = ()> + Send + 'static) {
        let handle = AbortOnDropHandle::new(tokio::spawn(future));
        self.task_handles
            .lock()
            .expect("locking failed")
            .push(handle);
    }

    pub(crate) fn start_pkarr_id_publisher(&self, secret_id: RostraIdSecretKey) {
        self.spawn_task(PkarrIdPublisher::new(self, secret_id).run());
    }

    pub(crate) fn start_head_merger(&self, secret_id: RostraIdSecretKey) {
        self.spawn_task(HeadMerger::new(self, secret_id).run());
    }

    pub(crate) fn start_request_handler(&self) {
        self.spawn_task(RequestHandler::new(self, self.networking.endpoint.clone()).run());
    }

    pub(crate) fn start_head_update_broadcaster(&self) {
        self.spawn_task(
            crate::task::head_update_broadcaster::HeadUpdateBroadcaster::new(self).run(),
        );
    }
    pub(crate) fn start_missing_event_fetcher(&self) {
        self.spawn_task(MissingEventFetcher::new(self).run());
    }
    pub(crate) fn start_missing_event_content_fetcher(&self) {
        self.spawn_task(MissingEventContentFetcher::new(self).run());
    }

    pub(crate) fn start_new_head_fetcher(&self) {
        self.spawn_task(crate::task::new_head_fetcher::NewHeadFetcher::new(self).run());
    }

    pub(crate) fn start_poll_follower_head_updates(&self) {
        self.spawn_task(
            crate::task::poll_follower_head_updates::PollFollowerHeadUpdates::new(self).run(),
        );
    }

    pub(crate) fn start_poll_followee_head_updates(&self) {
        self.spawn_task(
            crate::task::poll_followee_head_updates::PollFolloweeHeadUpdates::new(self).run(),
        );
    }

    pub(crate) fn start_wot_head_sync(&self) {
        self.spawn_task(crate::task::wot_head_sync::WotHeadSync::new(self).run());
    }

    pub(crate) fn start_news_score_updater(&self) {
        self.spawn_task(crate::task::news_score_updater::NewsScoreUpdater::new(self).run());
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

    /// Subscribe to owned snapshots of the minimum current self-head
    /// representative.
    ///
    /// The retained value is a deterministic default and does not imply that
    /// the complete current head set is a singleton.
    pub fn self_head_subscribe(&self) -> CurrentState<Option<ShortEventId>> {
        self.db.self_head_subscribe()
    }

    /// Subscribe to newly accepted verified event content.
    pub fn new_content_subscribe(&self) -> broadcast::Receiver<VerifiedEventContent> {
        self.db.new_content_subscribe()
    }

    /// Subscribe to newly accepted social posts.
    pub fn new_posts_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::SocialPost)> {
        self.db.new_posts_subscribe()
    }

    /// Subscribe to newly accepted shoutbox messages.
    pub fn new_shoutbox_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::Shoutbox)> {
        self.db.new_shoutbox_subscribe()
    }

    /// Subscribe to lossy incremental exact new-head signals.
    ///
    /// Each signal names the accepted event that became a head. Consumers that
    /// lag must recover from the database's complete durable head set.
    pub fn new_heads_subscribe(&self) -> broadcast::Receiver<(RostraId, ShortEventId)> {
        self.db.new_heads_subscribe()
    }

    /// Subscribe to deduplicated identities whose event history is incomplete.
    pub fn ids_with_missing_events_subscribe(
        &self,
        capacity: usize,
    ) -> dedup_chan::Receiver<RostraId> {
        self.db.ids_with_missing_events_subscribe(capacity)
    }

    /// Access the identity's database and materialized views.
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Return the deterministic representative of the current self-head set.
    pub async fn events_head(&self) -> Option<ShortEventId> {
        self.db.get_self_current_head().await
    }

    /// Create a weak handle that does not keep this client running.
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    /// Access in-memory P2P connection state for debugging.
    pub fn p2p_state(&self) -> &P2PState {
        self.networking.p2p_state()
    }

    pub(crate) fn connection_cache(&self) -> &crate::connection_cache::ConnectionCache {
        self.networking.connection_cache()
    }

    pub(crate) fn networking(&self) -> &Arc<crate::net::ClientNetworking> {
        &self.networking
    }

    /// Returns our local Iroh node ID.
    pub fn local_iroh_id(&self) -> IrohNodeId {
        IrohNodeId::from_bytes(*self.networking.endpoint.id().as_bytes())
    }

    /// Resolve an identity's published transport and graph-head information.
    pub async fn resolve_id_data(&self, id: RostraId) -> IdResolveResult<IdResolvedData> {
        self.networking.resolve_id_data(id).await
    }

    /// Resolve an identity's compact transport ticket.
    pub async fn resolve_id_ticket(&self, id: RostraId) -> IdResolveResult<CompactTicket> {
        self.networking.resolve_id_ticket(id).await
    }

    /// Open a fresh typed RPC connection to an identity.
    pub async fn connect_uncached(&self, id: RostraId) -> ConnectResult<Connection> {
        self.networking.connect_uncached(id).await
    }

    /// Resolve an identity through Pkarr and open a fresh typed RPC connection.
    pub async fn connect_by_pkarr_resolution(&self, id: RostraId) -> ConnectResult<Connection> {
        self.networking.connect_by_pkarr_resolution(id).await
    }

    /// Open a typed RPC connection using a previously resolved compact ticket.
    pub async fn connect_ticket(&self, ticket: CompactTicket) -> ConnectResult<Connection> {
        self.networking.connect_ticket(ticket).await
    }

    /// Fetch an event and its content from its author or known followers.
    ///
    /// The caller-owned cache avoids repeating follower lookups while fetching
    /// a batch of related events. The method returns `true` when content
    /// was found and stored.
    pub async fn fetch_event_content(
        &self,
        author_id: RostraId,
        event_id: ShortEventId,
        followers_cache: &mut std::collections::BTreeMap<RostraId, Vec<RostraId>>,
    ) -> Result<bool, DbError> {
        crate::util::rpc::get_event_content_from_followers(
            self.networking(),
            self.rostra_id(),
            author_id,
            event_id,
            self.connection_cache(),
            followers_cache,
            self.db(),
        )
        .await
    }

    /// Synchronize an event and its ancestors from a set of candidate peers.
    ///
    /// This explicit synchronization boundary is useful when an integration
    /// learns a remote head outside the client's normal background discovery.
    /// It returns `true` when any new envelope or content was stored.
    pub async fn sync_event_from_peers(
        &self,
        author_id: RostraId,
        head: ShortEventId,
        peers: &[RostraId],
    ) -> Result<bool, DbError> {
        crate::util::rpc::download_events_from_child(
            author_id,
            head,
            self.networking(),
            self.connection_cache(),
            peers,
            self.db(),
        )
        .await
    }

    pub(crate) fn pkarr_client(&self) -> Arc<pkarr::Client> {
        self.networking.pkarr_client.clone()
    }

    pub(crate) async fn does_have_event(&self, _event_id: rostra_core::EventId) -> bool {
        // TODO: check
        false
    }

    /// Store verified event content, retaining author and event context on
    /// error.
    pub async fn store_event_with_content(
        &self,
        event_id: impl Into<ShortEventId>,
        content: &VerifiedEventContent,
    ) -> StoreEventResult<()> {
        let event_id = event_id.into();
        self.db
            .try_process_event_with_content(content)
            .await
            .map(|_| ())
            .map_err(|source| StoreEventError {
                author_id: content.author(),
                event_id,
                source,
            })
    }

    /// Store an oversized verified envelope without accepting its payload.
    pub(crate) async fn store_event_too_large(&self, event: &VerifiedEvent) -> DbResult<()> {
        self.db.try_process_event(event).await.map(|_| ())
    }

    pub(crate) fn event_size_limit(&self) -> u32 {
        // TODO: take from db or something
        16 * 1024 * 1024
    }

    /// Read and parse a secret identity key from a UTF-8 file.
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
        self.db
            .try_process_event_with_content(&verified_event_content)
            .await
            .context(StorageSnafu)?;

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
        persona_tags: BTreeSet<PersonaTag>,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::SocialPost::new(body, reply_to, persona_tags),
        )
        .call()
        .await
    }

    pub async fn social_news_post(
        &self,
        id_secret: RostraIdSecretKey,
        body: String,
        url: Option<url::Url>,
        title: Option<String>,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::SocialPost::new(
                body,
                None,
                BTreeSet::from([PersonaTag::new("news").expect("valid persona tag")]),
            )
            .with_news_fields(url, title),
        )
        .call()
        .await
    }

    pub async fn set_social_vote(
        &self,
        id_secret: RostraIdSecretKey,
        post_id: ExternalEventId,
        upvote: Option<bool>,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(id_secret, content_kind::SocialVote::new(post_id, upvote))
            .call()
            .await
    }

    pub async fn get_self_social_vote(&self, post_id: ExternalEventId) -> Option<Option<bool>> {
        self.db.get_social_vote(self.rostra_id(), post_id).await
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
        tags_selector: PersonasTagsSelector,
    ) -> PostResult<VerifiedEvent> {
        self.publish_event(
            id_secret,
            content_kind::Follow {
                followee: followee_id,
                selector: None,
                persona: None,
                persona_tags_selector: Some(tags_selector),
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
                persona_tags_selector: None,
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
                            let (event, content) = Event::builder(&SocialPost::new(
                                body.clone(),
                                None,
                                BTreeSet::new(),
                            ))
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

    /// Subscribe to owned snapshots of the retained self-followee projection.
    pub fn self_followees_subscribe(
        &self,
    ) -> CurrentState<Arc<HashMap<RostraId, IdsFolloweesRecord>>> {
        self.db.self_followees_subscribe()
    }

    /// Subscribe to owned snapshots of the retained self-follower projection.
    pub fn self_followers_subscribe(
        &self,
    ) -> CurrentState<Arc<HashMap<RostraId, IdsFollowersRecord>>> {
        self.db.self_followers_subscribe()
    }

    /// Subscribe to owned snapshots of the retained Web-of-Trust projection.
    pub fn self_wot_subscribe(&self) -> CurrentState<Arc<WotData>> {
        self.db.self_wot_subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering::SeqCst;
    use std::time::Duration;

    use iroh::endpoint::presets;
    use rostra_client_db::{Database, DbError, EventContentState};
    use rostra_core::event::{Event, EventContentRaw, EventKind};
    use rostra_core::id::{RostraIdSecretKey, ToShort as _};
    use rostra_p2p::connection::FeedEventResponse;
    use rostra_p2p::{Connection, RpcError};
    use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;

    use super::Client;
    use crate::error::PostError;

    #[test_log::test(tokio::test(flavor = "multi_thread"))]
    async fn failed_announcement_does_not_commit_activation_and_retry_can_start_tasks() {
        let secret = RostraIdSecretKey::from_bytes([51; 32]);
        let endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .bind()
            .await
            .expect("test endpoint");
        let client = Client::builder(secret.id())
            .iroh_endpoint(endpoint)
            .start_request_handler(false)
            .start_background_tasks(false)
            .build()
            .await
            .expect("test client");

        client
            .finish_activation(
                secret,
                Err(PostError::Storage {
                    source: DbError::Overflow,
                }),
            )
            .expect_err("announcement storage failure");
        assert!(!client.active.load(SeqCst));
        assert!(client.task_handles.lock().expect("task handles").is_empty());

        client
            .finish_activation(secret, Ok(()))
            .expect("activation retry");
        assert!(client.active.load(SeqCst));
        assert_eq!(
            client.task_handles.lock().expect("task handles").len(),
            2,
            "the retry starts each signing task exactly once"
        );
    }

    #[test_log::test(tokio::test(flavor = "multi_thread"))]
    async fn oversized_feed_event_retains_pruned_envelope() {
        let secret = RostraIdSecretKey::from_bytes([52; 32]);
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();
        let server_endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("server endpoint");
        let server_id = server_endpoint.id();
        lookup.add_endpoint_info(server_endpoint.addr());
        let client = Client::builder(secret.id())
            .db(Database::new_in_memory(secret.id())
                .await
                .expect("in-memory database"))
            .iroh_endpoint(server_endpoint)
            .start_background_tasks(false)
            .build()
            .await
            .expect("server client");
        let caller_endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .address_lookup(lookup)
            .bind()
            .await
            .expect("caller endpoint");
        let connection = Connection::from(
            caller_endpoint
                .connect(server_id, ROSTRA_P2P_V0_ALPN)
                .await
                .expect("connect to server"),
        );
        let content = EventContentRaw::new(vec![0; client.event_size_limit() as usize + 1]);
        let event = Event::builder_raw_content()
            .author(secret.id())
            .kind(EventKind::NULL)
            .content(&content)
            .build()
            .signed_by(secret);
        let event_id = event.event.compute_id().to_short();

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            connection.feed_event(event, content),
        )
        .await
        .expect("oversized event response")
        .expect_err("oversized event must not transfer its content");

        assert!(matches!(
            error,
            RpcError::Failed {
                return_code: FeedEventResponse::RETURN_CODE_ALREADY_HAVE
            }
        ));
        assert_eq!(
            client.db().get_event_content_state(event_id).await,
            Some(EventContentState::Pruned)
        );
    }
}
