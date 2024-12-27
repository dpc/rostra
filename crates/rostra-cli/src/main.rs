use std::io;
use std::time::Duration;

use rostra_client::Client;
use snafu::{FromString, ResultExt, Whatever};
use tracing::info;
use tracing_subscriber::EnvFilter;

pub const PROJECT_NAME: &str = "rostra";

type WhateverResult<T> = std::result::Result<T, snafu::Whatever>;

#[snafu::report]
#[tokio::main]
async fn main() -> WhateverResult<()> {
    init_logging()?;

    let client = Client::new()
        .await
        .whatever_context("Can't initialize client")?;

    loop {
        let rostra_id = client.rostra_id();
        match client.resolve_id(rostra_id).await {
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

pub fn init_logging() -> WhateverResult<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|_| Whatever::without_source("I".to_string()))?;

    Ok(())
}
