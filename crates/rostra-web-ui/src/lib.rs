pub(crate) mod asset_cache;
mod error;
mod fragment;
pub mod html_utils;
pub mod is_htmx;
mod routes;

use std::net::{AddrParseError, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;
use std::{io, result};

use asset_cache::AssetCache;
use axum::http::header::{ACCEPT, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::{middleware, Router};
use rostra_client::error::{ActivateError, IdSecretReadError, InitError};
use rostra_client::multiclient::{MultiClient, MultiClientError};
use rostra_client::{ClientHandle, ClientRefError};
use rostra_client_db::DbError;
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::{BoxedError, WhateverResult};
use routes::cache_control;
use snafu::{ensure, ResultExt as _, Snafu, Whatever};
use tokio::net::{TcpListener, TcpSocket};
use tokio::signal;
use tower_http::compression::predicate::SizeAbove;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::CompressionLevel;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};
use tracing::info;

pub const UI_ROOT_PATH: &str = "/ui";

const LOG_TARGET: &str = "rostra::web_ui";

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
}

impl Opts {
    pub fn new(
        listen: String,
        cors_origin: Option<String>,
        assets_dir: Option<PathBuf>,
        reuseport: bool,
        data_dir: PathBuf,
        secret_file: Option<PathBuf>,
    ) -> Self {
        Self {
            listen,
            cors_origin,
            assets_dir: assets_dir.unwrap_or_else(default_rostra_assets_dir),
            reuseport,
            data_dir,
            secret_file,
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

#[derive(Debug, Snafu)]
pub enum UiStateClientUnlockError {
    IdMismatch,
    InvalidMnemonic {
        source: BoxedError,
    },
    #[snafu(transparent)]
    Io {
        source: io::Error,
    },
    Database {
        source: DbError,
    },
    Init {
        source: InitError,
    },
    #[snafu(transparent)]
    MultiClient {
        source: MultiClientError,
    },
    #[snafu(transparent)]
    MultiClientActivate {
        source: ActivateError,
    },
}
pub type UiStateClientUnlockResult<T> = result::Result<T, UiStateClientUnlockError>;

pub struct UiState {
    clients: MultiClient,
}

impl UiState {
    pub async fn client(&self, id: RostraId) -> UiStateClientResult<ClientHandle> {
        if let Some(handle) = self.clients.get(id).await {
            Ok(handle)
        } else {
            ClientNotLoadedSnafu.fail()
        }
    }

    pub async fn unlock(
        &self,
        rostra_id: RostraId,
        mnemonic: &str,
    ) -> UiStateClientUnlockResult<Option<RostraIdSecretKey>> {
        let res = if mnemonic.trim().is_empty() {
            self.clients.load(rostra_id).await?;
            None
        } else {
            let secret_id = RostraIdSecretKey::from_str(mnemonic)
                .boxed()
                .context(InvalidMnemonicSnafu)?;

            ensure!(secret_id.id() == rostra_id, IdMismatchSnafu);
            let client = self.clients.load(rostra_id).await?;
            client.unlock_active(secret_id)?;

            Some(secret_id)
        };
        Ok(res)
    }
}

pub type SharedState = Arc<UiState>;
pub struct Server {
    listener: TcpListener,

    state: SharedState,
    assets: Option<AssetCache>,
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
        source: UiStateClientUnlockError,
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
            AssetCache::load_files(&opts.assets_dir)
                .await
                .context(AssetsLoadSnafu)?
                .into()
        };
        let state = Arc::new(UiState { clients });

        info!("Listening on {}", listener.local_addr()?);
        Ok(Self {
            listener,
            state,
            opts,
            assets,
        })
    }

    pub async fn get_listener(opts: &Opts) -> ServerResult<TcpListener> {
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

        if let Some(assets) = self.assets {
            router = router.nest("/assets", routes::static_file_handler(assets));
        } else {
            router = router.nest_service(
                "/assets",
                ServeDir::new(format!("{}/assets", env!("CARGO_MANIFEST_DIR"))),
            );
        }

        let session_store = MemoryStore::default();
        let session_layer = SessionManagerLayer::new(session_store)
            .with_expiry(Expiry::OnInactivity(time::Duration::minutes(30)));

        axum::serve(
            self.listener,
            router
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
        .quality(CompressionLevel::Precise(4))
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
