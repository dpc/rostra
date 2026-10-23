use std::sync::Arc;

use rostra_client_db::social_post_mutation::{
    SocialPostMutation, SocialPostMutationRequest, SocialPostMutationRequestId,
};
use rostra_client_db::social_post_request::{SocialPostRequest, SocialPostRequestId};
use rostra_client_db::{Database, DbError, EventContentAvailability};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn mutation_public_api_reconcile_after_delete_reopen_and_shutdown() -> anyhow::Result<()> {
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
    let original = client
        .social_post_idempotent(
            secret,
            SocialPostRequestId([1; 16]),
            &SocialPostRequest {
                body: "original".into(),
                reply_to: None,
                persona_tags: Default::default(),
            },
        )
        .await?;
    let input = SocialPostMutationRequest {
        expected_current: original.event_id,
        mutation: SocialPostMutation::Edit {
            body: " ❤️ \n".into(),
        },
    };
    let id = SocialPostMutationRequestId([1; 16]);
    let (first, second) = tokio::join!(
        client.mutate_social_post_idempotent(secret, id, &input),
        client.mutate_social_post_idempotent(secret, id, &input),
    );
    let edit = first?;
    assert_eq!(edit.event_id, second?.event_id);
    assert_eq!(
        client
            .db
            .get_social_post(edit.event_id.to_short())
            .await
            .unwrap()
            .content
            .djot_content
            .as_deref(),
        Some(" ❤️ \n")
    );
    assert!(matches!(
        client
            .mutate_social_post_idempotent(secret, SocialPostMutationRequestId([2; 16]), &input)
            .await,
        Err(PostError::Storage {
            source: DbError::SocialPostMutationConflict
        })
    ));
    let delete = SocialPostMutationRequest {
        expected_current: edit.event_id,
        mutation: SocialPostMutation::Delete,
    };
    let deleted = client
        .mutate_social_post_idempotent(secret, SocialPostMutationRequestId([2; 16]), &delete)
        .await?;
    assert!(
        client
            .db
            .get_social_post_state(deleted.event_id.to_short())
            .await
            .unwrap()
            .is_tombstone
    );
    assert_eq!(
        client
            .mutate_social_post_idempotent(secret, id, &input)
            .await?
            .event_id,
        edit.event_id
    );
    let mut shutdown = Arc::clone(&client).begin_shutdown().unwrap();
    assert!(matches!(
        client
            .mutate_social_post_idempotent(secret, id, &input)
            .await,
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
        client
            .mutate_social_post_idempotent(secret, id, &input)
            .await?
            .event_id,
        edit.event_id
    );
    assert_eq!(
        client
            .db
            .get_event_content_availability(edit.event_id)
            .await,
        Some(EventContentAvailability::Deleted)
    );
    let mut shutdown = Arc::clone(&client).begin_shutdown().unwrap();
    drop(client);
    shutdown.join().await?;
    endpoint.close().await;
    Ok(())
}
