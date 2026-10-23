use std::sync::atomic::Ordering::SeqCst;

use rostra_client_db::{Database, DbError};
use rostra_core::event::EventExt as _;
use rostra_core::id::RostraIdSecretKey;

use super::Client;
use crate::error::{ActivateError, PostError};

#[tokio::test(flavor = "multi_thread")]
async fn managed_head_commit_survives_shutdown_before_activation() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("activation.redb");
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
    let event = client
        .social_post_if_head(secret, None, "first".to_owned(), None, Default::default())
        .await?;
    assert_eq!(event.parent_prev(), None);
    assert_eq!(event.parent_aux(), None);
    assert!(!client.active.load(SeqCst));
    let mut closing = client.clone().begin_shutdown().unwrap();
    assert!(matches!(
        client.unlock_active(secret).await,
        Err(ActivateError::ActivationClosing)
    ));
    drop(client);
    closing.join().await?;
    let client = Client::builder(secret.id())
        .db(Database::open(&path, secret.id()).await?)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await?;
    assert!(matches!(
        client
            .social_post_if_head(secret, None, "retry".to_owned(), None, Default::default())
            .await,
        Err(PostError::Storage {
            source: DbError::SocialPostHeadConflict
        })
    ));
    assert!(client.db.get_event(event.event_id).await.is_some());
    client.unlock_active(secret).await?;
    assert!(client.active.load(SeqCst));
    client.begin_shutdown().unwrap().join().await?;
    endpoint.close().await;
    Ok(())
}
