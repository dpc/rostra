mod asset_cache;
mod error;
mod fragment;
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
use axum::Router;
use rostra_client::error::{IdSecretReadError, InitError};
use rostra_client::{Client, ClientHandle, ClientRefError};
use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraIdSecretKey;
use rostra_util_error::{BoxedError, WhateverResult};
use snafu::{ResultExt as _, Snafu, Whatever};
use tokio::net::{TcpListener, TcpSocket};
use tokio::signal;
use tower_http::compression::predicate::SizeAbove;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::CompressionLevel;
use tracing::info;

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
    AlreadyLoaded,
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
}
pub type UiStateClientUnlockResult<T> = result::Result<T, UiStateClientUnlockError>;

pub struct UiState {
    client: tokio::sync::RwLock<Option<Arc<Client>>>,
    pub assets: AssetCache,
    opts: Opts,
}

impl UiState {
    pub async fn client(&self) -> UiStateClientResult<ClientHandle> {
        if let Some(read) = self.client.read().await.as_ref() {
            Ok(read.handle())
        } else {
            ClientNotLoadedSnafu.fail()
        }
    }

    pub async fn unlock(&self, mnemonic: &str) -> UiStateClientUnlockResult<()> {
        let secret_id = RostraIdSecretKey::from_str(mnemonic)
            .boxed()
            .context(InvalidMnemonicSnafu)?;
        self.unlock_secret(secret_id).await
    }
    pub async fn unlock_secret(
        &self,
        secret_id: RostraIdSecretKey,
    ) -> UiStateClientUnlockResult<()> {
        let mut write = self.client.write().await;
        if write.is_some() {
            return AlreadyLoadedSnafu.fail();
        }

        let self_id = secret_id.id();
        let client = Client::builder()
            .id_secret(secret_id)
            .db(Database::open(
                Database::mk_db_path(&self.opts.data_dir, self_id).await?,
                self_id,
            )
            .await
            .context(DatabaseSnafu)?)
            .build()
            .await
            .context(InitSnafu)?;

        *write = Some(client);
        Ok(())
    }
}

pub type SharedState = Arc<UiState>;
pub struct Server {
    listener: TcpListener,

    state: SharedState,
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
    pub async fn init(opts: Opts) -> ServerResult<Server> {
        let listener = Self::get_listener(&opts).await?;

        let assets = AssetCache::load_files(&opts.assets_dir)
            .await
            .context(AssetsLoadSnafu)?;
        let state = Arc::new(UiState {
            assets,
            client: tokio::sync::RwLock::new(None),
            opts: opts.clone(),
        });

        if let Some(secret_file) = opts.secret_file.clone() {
            let secret_id = Client::read_id_secret(&secret_file)
                .await
                .context(SecretSnafu)?;

            state
                .unlock_secret(secret_id)
                .await
                .context(SecretUnlockSnafu)?;
        };

        info!("Listening on {}", listener.local_addr()?);
        Ok(Self {
            listener,
            state,
            opts,
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
        let mut router = Router::new().merge(routes::route_handler(self.state.clone()));

        if std::env::var("ROSTRA_DEV_MODE").is_ok() {
            router = router.nest_service(
                "/assets",
                ServeDir::new(format!("{}/assets", env!("CARGO_MANIFEST_DIR"))),
            );
        } else {
            router = router.nest("/assets", routes::static_file_handler(self.state.clone()));
        }

        info!("Starting server");
        let listen = self.addr()?;
        axum::serve(
            self.listener,
            router
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
        .allow_origin(opts.cors_origin(listen).context(CorsSnafu)?)
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
    pub fn cors_origin(&self, listen: SocketAddr) -> WhateverResult<HeaderValue> {
        self.cors_origin
            .clone()
            .unwrap_or_else(|| format!("http://{}", listen))
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
