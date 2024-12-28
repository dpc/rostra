mod cli;

use std::io;
use std::time::Duration;

use clap::Parser;
use cli::Opts;
use futures::future::pending;
use rostra_client::{Client, ConnectError, IdResolveError, InitError};
use rostra_p2p::connection::PingRequest;
use rostra_p2p::RpcError;
use snafu::{FromString, ResultExt, Snafu, Whatever};
use tracing::info;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

pub const PROJECT_NAME: &str = "rostra";

type WhateverResult<T> = std::result::Result<T, snafu::Whatever>;

#[derive(Debug, Snafu)]
pub enum CliError {
    Init { source: InitError },
    Resolve { source: IdResolveError },
    Connect { source: ConnectError },
    Rpc { source: RpcError },
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
            println!("{}", serde_json::to_string(&v).expect("Can't fail"));
            Ok(())
        }
        Err(err) => Err(err),
    }
}

async fn handle_cmd(opts: Opts) -> CliResult<serde_json::Value> {
    match opts.cmd {
        cli::OptsCmd::Dev(cmd) => match cmd {
            cli::DevCmd::ResolveId { id } => {
                let client = Client::new().await.context(InitSnafu)?;

                let out = client.resolve_id_data(id).await.context(ResolveSnafu)?;

                Ok(serde_json::to_value(out).expect("Can't fail"))
            }
            cli::DevCmd::Test => {
                let client = Client::new().await.context(InitSnafu)?;

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
            cli::DevCmd::Ping { id, seq } => {
                let client = Client::new().await.context(InitSnafu)?;
                let connection = client.connect(id).await.context(ConnectSnafu)?;

                let resp = connection
                    .make_rpc(&PingRequest(seq))
                    .await
                    .context(RpcSnafu)?;

                Ok(serde_json::to_value(&resp).expect("Can't fail"))
            }
        },
        cli::OptsCmd::Serve => {
            let _client = Client::new().await.context(InitSnafu)?;

            pending().await
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
