use std::sync::Arc;

use rostra_client_db::news_post_mutation::{
    NewsPostMutation, NewsPostMutationRequest, NewsPostMutationRequestId,
};
use rostra_client_db::news_post_request::{NewsPostRequest, NewsPostRequestId};
use rostra_client_db::{Database, DbError, EventContentAvailability};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_public_api_reconcile_reopen_and_shutdown() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("news-mutation.redb");
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
        .social_news_post_idempotent(
            secret,
            NewsPostRequestId([1; 16]),
            &NewsPostRequest {
                body: String::new(),
                title: "Title only".into(),
                url: None,
            },
        )
        .await?;
    let input = NewsPostMutationRequest {
        expected_current: original.event_id,
        mutation: NewsPostMutation::Edit {
            body: " ❤️ \n".into(),
        },
    };
    let id = NewsPostMutationRequestId([1; 16]);
    let (first, second) = tokio::join!(
        client.mutate_news_post_idempotent(secret, id, &input),
        client.mutate_news_post_idempotent(secret, id, &input),
    );
    let edit = first?;
    assert_eq!(edit.event_id, second?.event_id);
    let post = client.db.prepare_news_post_mutation(edit.event_id).await?;
    assert_eq!(post.djot_content.as_deref(), Some(" ❤️ \n"));
    assert_eq!(post.title.as_deref(), Some("Title only"));
    assert!(post.news);
    assert!(matches!(
        client
            .mutate_news_post_idempotent(secret, NewsPostMutationRequestId([2; 16]), &input)
            .await,
        Err(PostError::Storage {
            source: DbError::SocialPostMutationConflict
        })
    ));
    let delete = NewsPostMutationRequest {
        expected_current: edit.event_id,
        mutation: NewsPostMutation::Delete,
    };
    let deletion = client
        .mutate_news_post_idempotent(secret, NewsPostMutationRequestId([2; 16]), &delete)
        .await?;
    assert!(
        client
            .db
            .get_social_post_state(deletion.event_id.to_short())
            .await
            .unwrap()
            .is_tombstone
    );
    assert_eq!(
        client
            .mutate_news_post_idempotent(secret, id, &input)
            .await?
            .event_id,
        edit.event_id
    );
    assert!(matches!(
        client
            .mutate_news_post_idempotent(RostraIdSecretKey::generate(), id, &input)
            .await,
        Err(PostError::Storage {
            source: DbError::SocialPostMutationNotOwner
        })
    ));
    let mut shutdown = Arc::clone(&client).begin_shutdown().unwrap();
    assert!(matches!(
        client.mutate_news_post_idempotent(secret, id, &input).await,
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
            .mutate_news_post_idempotent(secret, id, &input)
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
