mod cli;

use std::io;
use std::time::Duration;

use clap::Parser;
use cli::Opts;
use futures::future::pending;
use rostra_client::error::{ConnectError, IdResolveError, IdSecretReadError, InitError, PostError};
use rostra_client::Client;
use rostra_core::id::RostraIdSecretKey;
use rostra_p2p::connection::{Connection, PingRequest, PingResponse};
use rostra_p2p::RpcError;
use rostra_util_error::FmtCompact as _;
use snafu::{FromString, ResultExt, Snafu, Whatever};
use tokio::time::Instant;
use tracing::info;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

pub const PROJECT_NAME: &str = "rostra";
pub const LOG_TARGET: &str = "rostra::cli";

type WhateverResult<T> = std::result::Result<T, snafu::Whatever>;

#[derive(Debug, Snafu)]
pub enum CliError {
    #[snafu(display("Initialization error: {source}"))]
    Init { source: InitError },
    #[snafu(display("ID resolution error: {source}"))]
    Resolve { source: IdResolveError },
    #[snafu(display("Connection error: {source}"))]
    Connect { source: ConnectError },
    #[snafu(display("RPC error: {source}"))]
    Rpc { source: RpcError },
    #[snafu(display("Secret read error: {source}"))]
    Secret { source: IdSecretReadError },
    #[snafu(transparent)]
    Post { source: PostError },
    #[snafu(display("Miscellaneous error: {source}"))]
    Whatever { source: Whatever },
}

pub type CliResult<T> = std::result::Result<T, CliError>;

#[snafu::report]
#[tokio::main]
async fn main() -> CliResult<()> {
    init_logging().context(WhateverSnafu)?;

    let opts = Opts::parse();
    match handle_cmd(opts).await {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).expect("Can't fail"));
            Ok(())
        }
        Err(err) => Err(err),
    }
}

async fn handle_cmd(opts: Opts) -> CliResult<serde_json::Value> {
    match opts.cmd {
        cli::OptsCmd::Dev(cmd) => match cmd {
            cli::DevCmd::ResolveId { id } => {
                let client = Client::builder().build().await.context(InitSnafu)?;

                let out = client.resolve_id_data(id).await.context(ResolveSnafu)?;

                Ok(serde_json::to_value(out).expect("Can't fail"))
            }
            cli::DevCmd::Test => {
                let client = Client::builder().build().await.context(InitSnafu)?;

                loop {
                    let rostra_id = client.rostra_id();
                    match client.resolve_id_data(rostra_id).await {
                        Ok(data) => {
                            info!(id = %rostra_id.try_fmt(), ?data, "ID resolved");
                        }
                        Err(err) => {
                            info!(%err, id = %rostra_id.try_fmt(), "Resolution error");
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            }
            cli::DevCmd::Ping {
                id,
                mut seq,
                count,
                connect_once,
            } => {
                async fn ping(
                    client: &Client,
                    connection: Option<&Connection>,
                    id: rostra_core::id::RostraId,
                    seq: u64,
                ) -> CliResult<PingResponse> {
                    if let Some(connection) = connection {
                        connection
                            .make_rpc(&PingRequest(seq))
                            .await
                            .context(RpcSnafu)
                    } else {
                        let connection = client.connect(id).await.context(ConnectSnafu)?;
                        connection
                            .make_rpc(&PingRequest(seq))
                            .await
                            .context(RpcSnafu)
                    }
                }
                let client = Client::builder()
                    .start_request_handler(false)
                    .start_id_publisher(false)
                    .build()
                    .await
                    .context(InitSnafu)?;
                let connection = if connect_once {
                    Some(client.connect(id).await.context(ConnectSnafu)?)
                } else {
                    None
                };

                let mut resp = None;

                for _ in 0..count {
                    let start = Instant::now();

                    let resp_res = ping(&client, connection.as_ref(), id, seq).await;

                    let rtt = start.elapsed();
                    match resp_res {
                        Ok(ok) => {
                            info!(target: LOG_TARGET, elapsed_usecs = rtt.as_micros(), seq=%serde_json::to_string(&ok).expect("Can't fail"), "Response");
                            resp = Some(ok);
                        }
                        Err(err) => {
                            info!(target: LOG_TARGET, elapsed_usecs = rtt.as_micros(), %seq, err=%err.fmt_compact(), "Error");
                        }
                    }

                    seq = seq.wrapping_add(1);
                }

                Ok(serde_json::to_value(&resp).expect("Can't fail"))
            }
        },
        cli::OptsCmd::Serve { secret_file } => {
            let secret_id = if let Some(secret_file) = secret_file {
                Some(
                    Client::read_id_secret(&secret_file)
                        .await
                        .context(SecretSnafu)?,
                )
            } else {
                None
            };
            let _client = Client::builder()
                .maybe_id_secret(secret_id)
                .build()
                .await
                .context(InitSnafu)?;

            pending().await
        }
        cli::OptsCmd::GenId => {
            let secret = RostraIdSecretKey::generate();
            let id = secret.id();

            Ok(serde_json::to_value(&serde_json::json!({
                "id": id,
                "secret": secret,
            }))
            .expect("Can't fail"))
        }
        cli::OptsCmd::Post { body, secret_file } => {
            let id_secret = Client::read_id_secret(&secret_file)
                .await
                .context(SecretSnafu)?;

            let client = Client::builder()
                .id_secret(id_secret)
                .start_request_handler(false)
                .start_id_publisher(false)
                .build()
                .await
                .context(InitSnafu)?;

            client.post(body).await?;

            Ok(serde_json::Value::Bool(true))
        }
    }
}

pub fn init_logging() -> WhateverResult<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .try_init()
        .map_err(|_| Whatever::without_source("Failed to initialize logging".to_string()))?;

    Ok(())
}
