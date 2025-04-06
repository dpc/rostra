pub(crate) mod asset_cache;
mod error;
mod fragment;
pub mod html_utils;
pub mod is_htmx;
mod routes;
// TODO: move to own crate
mod serde_util;

use std::net::{AddrParseError, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;
use std::{io, result};

use asset_cache::AssetCache;
use axum::http::header::{ACCEPT, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::routing::get;
use axum::{Router, middleware};
use error::{IdMismatchSnafu, UnlockError, UnlockResult};
use listenfd::ListenFd;
use rostra_client::error::IdSecretReadError;
use rostra_client::multiclient::MultiClient;
use rostra_client::{ClientHandle, ClientRefError};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::WhateverResult;
use routes::{cache_control, get_static_asset};
use snafu::{ResultExt as _, Snafu, Whatever, ensure};
use tokio::net::{TcpListener, TcpSocket};
use tokio::signal;
use tower_cookies::CookieManagerLayer;
use tower_http::CompressionLevel;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::SizeAbove;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};
use tracing::info;

pub const UI_ROOT_PATH: &str = "/ui";

const LOG_TARGET: &str = "rostra::web_ui";

/// Handles ETag-based conditional requests
///
/// Takes the request headers, the ETag value, and response headers to modify.
/// If the client already has the current version (based on If-None-Match
/// header), returns a 304 Not Modified response.
///
/// Returns:
/// - Some(Response) if a 304 Not Modified should be returned
/// - None if processing should continue normally
pub fn handle_etag(
    req_headers: &axum::http::HeaderMap,
    etag: &str,
    resp_headers: &mut axum::http::HeaderMap,
) -> Option<axum::response::Response> {
    use axum::http::StatusCode;
    use axum::http::header::{ETAG, IF_NONE_MATCH};
    use axum::response::IntoResponse;

    // Add ETag header to response
    if let Ok(etag_value) = axum::http::HeaderValue::from_str(etag) {
        resp_headers.insert(ETAG, etag_value);
    }

    // Check if client already has this version
    if let Some(if_none_match) = req_headers.get(IF_NONE_MATCH) {
        if if_none_match.as_bytes() == etag.as_bytes() {
            return Some((StatusCode::NOT_MODIFIED, resp_headers.clone()).into_response());
        }
    }

    None
}

fn default_rostra_assets_dir() -> PathBuf {
    PathBuf::from(env!("ROSTRA_SHARE_DIR")).join("assets")
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub listen: String,
    pub cors_origin: Option<String>,
    pub secret_file: Option<PathBuf>,
    assets_dir: PathBuf,
    pub reuseport: bool,
    pub data_dir: PathBuf,
    pub default_profile: Option<RostraId>,
}

impl Opts {
    pub fn new(
        listen: String,
        cors_origin: Option<String>,
        assets_dir: Option<PathBuf>,
        reuseport: bool,
        data_dir: PathBuf,
        secret_file: Option<PathBuf>,
        default_profile: Option<RostraId>,
    ) -> Self {
        Self {
            listen,
            cors_origin,
            assets_dir: assets_dir.unwrap_or_else(default_rostra_assets_dir),
            reuseport,
            data_dir,
            secret_file,
            default_profile,
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
    assets: Option<Arc<AssetCache>>,
    default_profile: Option<RostraId>,
}

impl UiState {
    pub async fn client(&self, id: RostraId) -> UiStateClientResult<ClientHandle> {
        match self.clients.get(id).await {
            Some(handle) => Ok(handle),
            _ => ClientNotLoadedSnafu.fail(),
        }
    }

    pub async fn unlock(
        &self,
        rostra_id: RostraId,
        secret_id: Option<RostraIdSecretKey>,
    ) -> UnlockResult<Option<RostraIdSecretKey>> {
        let res = if let Some(secret_id) = secret_id {
            ensure!(secret_id.id() == rostra_id, IdMismatchSnafu);
            let client = self.clients.load(secret_id.id()).await?;
            client.unlock_active(secret_id).await?;

            Some(secret_id)
        } else {
            self.clients.load(rostra_id).await?;
            None
        };
        Ok(res)
    }
}

pub type SharedState = Arc<UiState>;
pub struct Server {
    listener: TcpListener,

    state: SharedState,
    assets: Option<Arc<AssetCache>>,
    opts: Opts,
}

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
        source: Whatever,
    },

    #[snafu(transparent)]
    ClientRef {
        source: ClientRefError,
    },
}

pub type ServerResult<T> = std::result::Result<T, WebUiServerError>;
impl Server {
    pub async fn init(opts: Opts, clients: MultiClient) -> ServerResult<Server> {
        let listener = Self::get_listener(&opts).await?;

        // Todo: allow disabling with a cmdline flag too?
        let assets = if is_rostra_dev_mode_set() {
            None
        } else {
            Some(Arc::new(
                AssetCache::load_files(&opts.assets_dir)
                    .await
                    .context(AssetsLoadSnafu)?,
            ))
        };
        let state = Arc::new(UiState {
            clients,
            assets: assets.clone(),
            default_profile: opts.default_profile,
        });

        info!("Listening on {}", listener.local_addr()?);
        Ok(Self {
            listener,
            state,
            opts,
            assets,
        })
    }

    pub async fn get_listener(opts: &Opts) -> ServerResult<TcpListener> {
        if let Some(listener) = ListenFd::from_env().take_tcp_listener(0)? {
            return Ok(TcpListener::from_std(listener)?);
        }
        let socket = {
            let addr = SocketAddr::from_str(&opts.listen).context(ListenAddrSnafu)?;

            let socket = if addr.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            if opts.reuseport {
                #[cfg(unix)]
                socket.set_reuseport(true)?;
            }
            socket.set_nodelay(true)?;

            socket.bind(addr)?;

            socket
        };

        Ok(socket.listen(1024)?)
    }

    pub async fn run(self) -> ServerResult<()> {
        let listen = self.listener.local_addr()?;
        info!(
            target: LOG_TARGET,
            addr = %listen,
            origin = %self.opts.cors_origin_url_str(listen),
            "Starting server"
        );
        let mut router = Router::new().merge(routes::route_handler(self.state.clone()));

        match self.assets {
            Some(_assets) => {
                router = router.nest("/assets", {
                    let state = self.state.clone();
                    Router::new()
                        .route("/{*file}", get(get_static_asset))
                        .with_state(state)
                });
            }
            _ => {
                router = router.nest_service(
                    "/assets",
                    ServeDir::new(format!("{}/assets", env!("CARGO_MANIFEST_DIR"))),
                );
            }
        }

        let session_store = MemoryStore::default();
        let session_layer = SessionManagerLayer::new(session_store)
            .with_expiry(Expiry::OnInactivity(time::Duration::minutes(2 * 24 * 60)));

        axum::serve(
            self.listener,
            router
                .with_state(self.state.clone())
                .layer(CookieManagerLayer::new())
                .layer(middleware::from_fn(cache_control))
                .layer(session_layer)
                .layer(cors_layer(&self.opts, listen)?)
                .layer(compression_layer())
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await?;

        Ok(())
    }

    pub fn addr(&self) -> ServerResult<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }
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
            .unwrap_or_else(|| format!("http://{}", listen))
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
