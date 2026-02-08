mod error;
pub mod html_utils;
mod layout;
mod routes;
mod secrets;
mod session_token;

pub(crate) use session_token::SessionToken;
// TODO: move to own crate
mod serde_util;
pub mod util;

use std::net::{AddrParseError, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{io, result};

use axum::http::header::{ACCEPT, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::{Router, middleware};
use axum_dpc_static_assets::{StaticAssetService, StaticAssets};
use error::{IdMismatchSnafu, UnlockError, UnlockResult};
use listenfd::ListenFd;
use rostra_client::error::IdSecretReadError;
use rostra_client::multiclient::MultiClient;
use rostra_client::{ClientHandle, ClientRefError};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_bind_addr::BindAddr;
use rostra_util_error::WhateverResult;
use routes::cache_control;
use snafu::{ResultExt as _, Snafu, Whatever, ensure};
use tokio::net::{TcpListener, TcpSocket, UnixListener};
use tokio::signal;
use tower_cookies::CookieManagerLayer;
use tower_http::CompressionLevel;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::SizeAbove;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_redb_store::{RedbSessionStore, SessionStoreError};
use tracing::info;

pub const UI_ROOT_PATH: &str = "/ui";

const LOG_TARGET: &str = "rostra::web_ui";

fn default_rostra_assets_dir() -> PathBuf {
    PathBuf::from(env!("ROSTRA_SHARE_DIR")).join("assets")
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub listen: BindAddr,
    pub cors_origin: Option<String>,
    assets_dir: PathBuf,
    pub reuseport: bool,
    pub data_dir: PathBuf,
    pub default_profile: Option<RostraId>,
    pub max_clients: usize,
    pub welcome_redirect: Option<String>,
}

impl Opts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        listen: BindAddr,
        cors_origin: Option<String>,
        assets_dir: Option<PathBuf>,
        reuseport: bool,
        data_dir: PathBuf,
        default_profile: Option<RostraId>,
        max_clients: usize,
        welcome_redirect: Option<String>,
    ) -> Self {
        Self {
            listen,
            cors_origin,
            assets_dir: assets_dir.unwrap_or_else(default_rostra_assets_dir),
            reuseport,
            data_dir,
            default_profile,
            max_clients,
            welcome_redirect,
        }
    }
}

impl Opts {
    pub fn assets_dir(&self) -> &Path {
        &self.assets_dir
    }
}

#[derive(Debug, Snafu)]
pub enum UiStateClientError {
    ClientNotLoaded,
    #[snafu(transparent)]
    ClientGone {
        source: ClientRefError,
    },
}
pub type UiStateClientResult<T> = result::Result<T, UiStateClientError>;

pub struct UiState {
    clients: MultiClient,
    assets: Option<Arc<StaticAssets>>,
    default_profile: Option<RostraId>,
    welcome_redirect: Option<String>,
    /// In-memory storage for secret keys.
    /// See [`secrets::SecretStore`] for details on the security design.
    secrets: secrets::SecretStore,
}

impl UiState {
    pub async fn client(&self, id: RostraId) -> UiStateClientResult<ClientHandle> {
        match self.clients.get(id).await {
            Some(handle) => Ok(handle),
            _ => ClientNotLoadedSnafu.fail(),
        }
    }

    /// Get the secret key for a session from in-memory storage.
    ///
    /// Takes the [`SessionToken`] as the key (derived from tower-sessions ID).
    /// Returns `None` if the user is in read-only mode (no secret key stored).
    pub fn id_secret(&self, session_token: SessionToken) -> Option<RostraIdSecretKey> {
        self.secrets.get(session_token)
    }

    /// Get the read-only mode status for a session.
    pub fn ro_mode(&self, session_token: SessionToken) -> routes::unlock::session::RoMode {
        if self.secrets.has_secret(session_token) {
            routes::unlock::session::RoMode::Rw
        } else {
            routes::unlock::session::RoMode::Ro
        }
    }

    /// Store or remove a secret key for a session.
    ///
    /// Call this after the session has been saved to the store, so that
    /// `session.id()` is available to create the `SessionToken`.
    pub fn set_session_secret(
        &self,
        session_token: SessionToken,
        secret: Option<RostraIdSecretKey>,
    ) {
        match secret {
            Some(s) => self.secrets.insert(session_token, s),
            None => self.secrets.remove(session_token),
        }
    }

    /// Load a client for read-only access (no secret key).
    ///
    /// Use this for default_profile or read-only unlock.
    pub async fn load_client(&self, rostra_id: RostraId) -> UnlockResult<()> {
        self.clients.load(rostra_id).await?;
        Ok(())
    }

    /// Unlock a client with optional secret key.
    ///
    /// This loads the client and unlocks it if a secret is provided.
    /// The caller is responsible for storing the secret in the session
    /// after the session has been saved (so session.id() is available).
    pub async fn unlock(
        &self,
        rostra_id: RostraId,
        secret_id: Option<RostraIdSecretKey>,
    ) -> UnlockResult<Option<RostraIdSecretKey>> {
        if let Some(secret_id) = secret_id {
            ensure!(secret_id.id() == rostra_id, IdMismatchSnafu);
            let client = self.clients.load(secret_id.id()).await?;
            client.unlock_active(secret_id).await?;
            Ok(Some(secret_id))
        } else {
            self.clients.load(rostra_id).await?;
            Ok(None)
        }
    }
}

pub type SharedState = Arc<UiState>;

#[derive(Debug, Snafu)]
pub enum WebUiServerError {
    #[snafu(transparent)]
    IO {
        source: io::Error,
    },

    Secret {
        source: IdSecretReadError,
    },

    SecretUnlock {
        source: UnlockError,
    },

    ListenAddr {
        source: AddrParseError,
    },

    Cors {
        source: Whatever,
    },

    AssetsLoad {
        source: axum_dpc_static_assets::LoadError,
    },

    #[snafu(transparent)]
    ClientRef {
        source: ClientRefError,
    },

    SessionStore {
        source: SessionStoreError,
    },
}

pub type ServerResult<T> = std::result::Result<T, WebUiServerError>;

pub async fn get_tcp_listener(addr: SocketAddr, reuseport: bool) -> ServerResult<TcpListener> {
    if let Some(listener) = ListenFd::from_env().take_tcp_listener(0)? {
        listener.set_nonblocking(true)?;
        return Ok(TcpListener::from_std(listener)?);
    }
    let socket = {
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        if reuseport {
            #[cfg(unix)]
            socket.set_reuseport(true)?;
        }
        socket.set_nodelay(true)?;

        socket.bind(addr)?;

        socket
    };

    Ok(socket.listen(1024)?)
}

pub async fn get_unix_listener(path: &Path) -> ServerResult<UnixListener> {
    // Remove existing socket file if it exists
    if path.exists() {
        std::fs::remove_file(path)?;
    }

    Ok(UnixListener::bind(path)?)
}

pub async fn run_ui(opts: Opts, clients: MultiClient) -> ServerResult<()> {
    // Todo: allow disabling with a cmdline flag too?
    let assets = if is_rostra_dev_mode_set() {
        None
    } else {
        Some(Arc::new(
            StaticAssets::load(&opts.assets_dir)
                .await
                .context(AssetsLoadSnafu)?,
        ))
    };

    let state = Arc::new(UiState {
        clients,
        assets: assets.clone(),
        default_profile: opts.default_profile,
        welcome_redirect: opts.welcome_redirect.clone(),
        secrets: secrets::SecretStore::new(),
    });

    // Create persistent session store with shared redb database
    let session_db_path = opts.data_dir.join("webui.redb");
    let session_db = tokio::task::spawn_blocking(move || {
        redb::Database::create(session_db_path)
            .map(redb_bincode::Database::from)
            .map(Arc::new)
    })
    .await
    .expect("spawn_blocking panicked")
    .map_err(SessionStoreError::from)
    .context(SessionStoreSnafu)?;

    let session_store = RedbSessionStore::new(session_db.clone()).context(SessionStoreSnafu)?;
    let session_layer = SessionManagerLayer::new(session_store)
        .with_name("rostra_session")
        .with_expiry(Expiry::OnInactivity(time::Duration::minutes(2 * 24 * 60)));

    match &opts.listen {
        BindAddr::Tcp(addr) => {
            let listener = get_tcp_listener(*addr, opts.reuseport).await?;
            let local_addr = listener.local_addr()?;

            info!(
                target: LOG_TARGET,
                listen = %local_addr,
                origin = %opts.cors_origin_url_str(local_addr),
                "Starting TCP server"
            );

            let mut router = Router::new().merge(routes::route_handler(state.clone()));
            router = match assets.clone() {
                Some(assets) => router.nest_service("/assets", StaticAssetService::new(assets)),
                _ => router.nest_service(
                    "/assets",
                    ServeDir::new(format!("{}/assets", env!("CARGO_MANIFEST_DIR"))),
                ),
            };

            axum::serve(
                listener,
                router
                    .with_state(state)
                    .layer(CookieManagerLayer::new())
                    .layer(middleware::from_fn(cache_control))
                    .layer(session_layer.clone())
                    .layer(cors_layer(&opts, local_addr)?)
                    .layer(compression_layer())
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        }
        BindAddr::Unix(path) => {
            let listener = get_unix_listener(path).await?;

            info!(
                target: LOG_TARGET,
                listen = %path.display(),
                "Starting Unix socket server"
            );

            let mut router = Router::new().merge(routes::route_handler(state.clone()));
            router = match assets.clone() {
                Some(assets) => router.nest_service("/assets", StaticAssetService::new(assets)),
                _ => router.nest_service(
                    "/assets",
                    ServeDir::new(format!("{}/assets", env!("CARGO_MANIFEST_DIR"))),
                ),
            };

            axum::serve(
                listener,
                router
                    .with_state(state)
                    .layer(CookieManagerLayer::new())
                    .layer(middleware::from_fn(cache_control))
                    .layer(session_layer)
                    .layer(compression_layer())
                    .into_make_service(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        }
    }

    Ok(())
}

fn compression_layer() -> CompressionLayer<SizeAbove> {
    CompressionLayer::new()
        .quality(CompressionLevel::Fastest)
        .br(true)
        .compress_when(SizeAbove::new(512))
}

fn cors_layer(opts: &Opts, listen: SocketAddr) -> ServerResult<CorsLayer> {
    Ok(CorsLayer::new()
        .allow_credentials(true)
        .allow_headers([ACCEPT, CONTENT_TYPE, HeaderName::from_static("csrf-token")])
        .max_age(Duration::from_secs(86400))
        .allow_origin(opts.cors_origin_url_header(listen).context(CorsSnafu)?)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::HEAD,
            Method::PATCH,
        ]))
}

impl Opts {
    pub fn cors_origin_url_str(&self, listen: SocketAddr) -> String {
        self.cors_origin
            .clone()
            .unwrap_or_else(|| format!("http://{listen}"))
    }
    pub fn cors_origin_domain_str(&self, listen: SocketAddr) -> String {
        self.cors_origin
            .clone()
            .unwrap_or_else(|| format!("{listen}"))
    }
    pub fn cors_origin_url_header(&self, listen: SocketAddr) -> WhateverResult<HeaderValue> {
        self.cors_origin_url_str(listen)
            .parse()
            .whatever_context("cors_origin does not parse as an http value")
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
