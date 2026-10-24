use rostra_client_db::media_request::{MediaRequest, MediaRequestId};
use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraIdSecretKey;
use rostra_core::static_media::normalize_static_media;

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn media_publication_retries_reopen_authority_and_shutdown() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("media.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let normalized = normalize_static_media(
        "image/webp",
        include_bytes!("../../../rostra-core/src/avatar/fixtures/red-3x2.webp"),
    )?;
    let request = MediaRequest {
        mime: "image/png".into(),
        data: normalized.data,
        policy_version: 1,
    };
    let id = MediaRequestId([1; 16]);
    let mut original = None;
    for _ in 0..2 {
        let local = Client::builder(secret.id())
            .db(Database::open(&path, secret.id()).await?)
            .iroh_endpoint(endpoint.clone())
            .start_background_tasks(false)
            .start_request_handler(false)
            .build()
            .await?;
        let (a, b) = tokio::join!(
            local.publish_media_idempotent(secret, id, &request),
            local.publish_media_idempotent(secret, id, &request)
        );
        let event = a?;
        assert_eq!(event.event_id, b?.event_id);
        if let Some(original) = original {
            assert_eq!(original, event.event_id);
        }
        original = Some(event.event_id);
        let (a, b) = tokio::join!(
            local.publish_media_idempotent(secret, MediaRequestId([2; 16]), &request),
            local.publish_media_idempotent(secret, MediaRequestId([3; 16]), &request)
        );
        assert_ne!(a?.event_id, b?.event_id);
        let page = local.db.list_own_media(None).await?;
        assert_eq!(page.items.len(), 3);
        assert!(
            page.items
                .iter()
                .any(|item| item.event_id == event.event_id)
        );
        let mut changed = request.clone();
        changed.policy_version = 999;
        assert!(matches!(
            local.publish_media_idempotent(secret, id, &changed).await,
            Err(PostError::Storage {
                source: DbError::MediaRequestMismatch,
                ..
            })
        ));
        assert!(
            local
                .publish_media_idempotent(secret, MediaRequestId([4; 16]), &changed)
                .await
                .is_err()
        );
        assert!(
            local
                .publish_media_idempotent(RostraIdSecretKey::generate(), id, &request)
                .await
                .is_err()
        );
        let mut shutdown = local.clone().begin_shutdown().unwrap();
        assert!(
            local
                .publish_media_idempotent(secret, id, &request)
                .await
                .is_err()
        );
        drop(local);
        shutdown.join().await?;
    }
    endpoint.close().await;
    Ok(())
}
