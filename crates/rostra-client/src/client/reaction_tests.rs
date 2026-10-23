use std::sync::Arc;

use rostra_client_db::reaction_request::{ReactionOperation, ReactionRequest, ReactionRequestId};
use rostra_client_db::social_post_request::{SocialPostRequest, SocialPostRequestId};
use rostra_client_db::{Database, EventContentAvailability};
use rostra_core::id::RostraIdSecretKey;

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn reactions_public_api_reconcile_remove_reopen_and_shutdown() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("client.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let client = Client::builder(secret.id())
        .db(Database::open(&path, secret.id()).await?)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await?;
    let target = client
        .social_post_idempotent(
            secret,
            SocialPostRequestId([1; 16]),
            &SocialPostRequest {
                body: "target".into(),
                reply_to: None,
                persona_tags: Default::default(),
            },
        )
        .await?;
    let input = ReactionRequest {
        target: (secret.id(), target.event_id),
        operation: ReactionOperation::Add {
            reaction: "❤️".into(),
        },
    };
    let id = ReactionRequestId([1; 16]);
    let (a, b) = tokio::join!(
        client.react_idempotent(secret, id, &input),
        client.react_idempotent(secret, id, &input)
    );
    let row = a?;
    assert_eq!(row.event_id, b?.event_id);
    let removal = ReactionRequest {
        operation: ReactionOperation::Remove {
            expected_current: row.event_id,
        },
        ..input.clone()
    };
    let removed = client
        .react_idempotent(secret, ReactionRequestId([2; 16]), &removal)
        .await?;
    assert_eq!(
        client.react_idempotent(secret, id, &input).await?.event_id,
        row.event_id
    );
    assert_eq!(
        client
            .db
            .reaction_summary(input.target, None, 10)
            .await?
            .total,
        0
    );
    let (independent_a, independent_b) = tokio::join!(
        client.react_idempotent(secret, ReactionRequestId([3; 16]), &input),
        client.react_idempotent(secret, ReactionRequestId([4; 16]), &input),
    );
    assert_ne!(independent_a?.event_id, independent_b?.event_id);
    assert_eq!(
        client
            .db
            .reaction_summary(input.target, None, 10)
            .await?
            .total,
        2
    );
    let mut shutdown = Arc::clone(&client).begin_shutdown().unwrap();
    assert!(matches!(
        client.react_idempotent(secret, id, &input).await,
        Err(PostError::ClientClosing)
    ));
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
        client.react_idempotent(secret, id, &input).await?.event_id,
        row.event_id
    );
    assert_eq!(
        client
            .react_idempotent(secret, ReactionRequestId([2; 16]), &removal)
            .await?
            .event_id,
        removed.event_id
    );
    assert_eq!(
        client.db.get_event_content_availability(row.event_id).await,
        Some(EventContentAvailability::Deleted)
    );
    let mut shutdown = Arc::clone(&client).begin_shutdown().unwrap();
    drop(client);
    shutdown.join().await?;
    endpoint.close().await;
    Ok(())
}
