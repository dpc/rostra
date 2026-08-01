//! Starts a relay-only, temporary client and then shuts it down by dropping it.

use rostra_client::{Client, RostraIdSecretKey};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = RostraIdSecretKey::generate();
    let client = Client::builder(secret.id())
        .secret(secret)
        .start_request_handler(false)
        .build()
        .await?;

    println!("started temporary client for {}", client.rostra_id());

    drop(client);
    Ok(())
}
