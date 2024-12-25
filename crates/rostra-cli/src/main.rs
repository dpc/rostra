use rostra_client::Client;
use rostra_core::id::RostraId;
use snafu::ResultExt;
use tracing::Level;

pub const PROJECT_NAME: &str = "rostra";

type WhateverResult<T> = std::result::Result<T, snafu::Whatever>;

#[snafu::report]
#[tokio::main]
async fn main() -> WhateverResult<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let _client = Client::new()
        .await
        .whatever_context("Can't initialize client")?;

    let other_id =
        RostraId::try_from_pkarr_str("qztjg1q9xmgu4uenfaxz4zx9pbr8dajg8anomcaqptxbqbptdk3y")
            .whatever_context("Can't parse id")?;

    // app.fetch_data(&other_id).await;

    std::future::pending::<()>().await;

    Ok(())
}
