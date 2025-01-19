mod asset_cache;
mod error;
mod fragment;
mod routes;

use std::io;
use std::net::{AddrParseError, SocketAddr};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use asset_cache::AssetCache;
use axum::http::header::{ACCEPT, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::Router;
use rostra_client::ClientHandle;
use snafu::{ResultExt as _, Snafu, Whatever};
use tokio::net::{TcpListener, TcpSocket};
use tokio::signal;
use tower_http::compression::predicate::SizeAbove;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::CompressionLevel;
use tracing::info;

use crate::cli::WebUiOpts;
use crate::WhateverResult;

pub struct AppState {
    client: ClientHandle,
    pub assets: AssetCache,
}

pub type SharedAppState = Arc<AppState>;
pub struct Server {
    listener: TcpListener,

    state: SharedAppState,
    opts: WebUiOpts,
}

#[derive(Debug, Snafu)]
pub enum WebUiServerError {
    #[snafu(transparent)]
    IO {
        source: io::Error,
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
}

pub type ServerResult<T> = std::result::Result<T, WebUiServerError>;
impl Server {
    pub async fn init(opts: WebUiOpts, client: ClientHandle) -> ServerResult<Server> {
        let listener = Self::get_listener(&opts).await?;

        let assets = AssetCache::load_files(&opts.assets_dir)
            .await
            .context(AssetsLoadSnafu)?;
        let state = Arc::new(AppState {
            client,
            assets,
            // req_counter: AtomicU64::default(),
        });

        info!("Listening on {}", listener.local_addr()?);
        Ok(Self {
            listener,
            state,
            opts,
        })
    }

    pub async fn get_listener(opts: &WebUiOpts) -> ServerResult<TcpListener> {
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
            router = router.nest_service("/assets", ServeDir::new("crates/rostra/assets/"));
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

fn cors_layer(opts: &WebUiOpts, listen: SocketAddr) -> ServerResult<CorsLayer> {
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

impl WebUiOpts {
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
