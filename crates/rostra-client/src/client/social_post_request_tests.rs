use std::sync::Arc;

use rostra_client_db::Database;
use rostra_client_db::social_post_request::{SocialPostRequest, SocialPostRequestId};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn social_post_idempotent_public_api_reopen_and_shutdown() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("client.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let input = SocialPostRequest {
        body: "a durable draft".to_owned(),
        reply_to: None,
        persona_tags: Default::default(),
    };
    let id = SocialPostRequestId([9; 16]);
    let client = Client::builder(secret.id())
        .db(Database::open(&path, secret.id()).await?)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await?;
    let (one, two) = tokio::join!(
        client.social_post_idempotent(secret, id, &input),
        client.social_post_idempotent(secret, id, &input),
    );
    let event = one?;
    assert_eq!(event.event_id, two?.event_id);
    let mut shutdown = client.clone().begin_shutdown().unwrap();
    assert!(matches!(
        client.social_post_idempotent(secret, id, &input).await,
        Err(PostError::ClientClosing)
    ));
    // Outstanding call/client ownership prevents successful close.
    assert!(shutdown.join().await.is_err());
    drop(client);
    shutdown.join().await?;
    let client = Client::builder(secret.id())
        .db(Database::open(&path, secret.id()).await?)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await?;
    assert_eq!(
        client
            .social_post_idempotent(secret, id, &input)
            .await?
            .event_id,
        event.event_id
    );
    let reply = SocialPostRequest {
        body: "❤️".to_owned(),
        reply_to: Some((secret.id(), event.event_id)),
        persona_tags: Default::default(),
    };
    let result = client
        .social_post_idempotent(secret, SocialPostRequestId([10; 16]), &reply)
        .await?;
    assert!(
        client
            .db
            .get_social_post(result.event_id.to_short())
            .await
            .is_some()
    );
    let mut changed = input.clone();
    changed.body.push('!');
    assert!(matches!(
        client.social_post_idempotent(secret, id, &changed).await,
        Err(PostError::Storage {
            source: rostra_client_db::DbError::SocialPostRequestMismatch
        })
    ));
    let mut shutdown = Arc::clone(&client).begin_shutdown().unwrap();
    drop(client);
    shutdown.join().await?;
    endpoint.close().await;
    Ok(())
}
