use rostra_client_db::shoutbox::{ShoutboxBounds, ShoutboxCursor};
use rostra_client_db::shoutbox_request::{ShoutboxRequest, ShoutboxRequestId};
use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraIdSecretKey;

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_public_concurrent_retries_authority_reopen_and_shutdown() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("shoutbox.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let input = ShoutboxRequest {
        body: "hello".into(),
    };
    let id = ShoutboxRequestId([1; 16]);
    let mut original = None;
    let mut cursor: Option<ShoutboxCursor> = None;
    for _ in 0..2 {
        let local = Client::builder(secret.id())
            .db(Database::open(&path, secret.id()).await?)
            .iroh_endpoint(endpoint.clone())
            .start_background_tasks(false)
            .start_request_handler(false)
            .build()
            .await?;
        let (a, b) = tokio::join!(
            local.post_shoutbox_idempotent(secret, id, &input),
            local.post_shoutbox_idempotent(secret, id, &input),
        );
        let result = a?.event_id;
        assert_eq!(result, b?.event_id);
        if let Some(original) = original {
            assert_eq!(result, original);
        }
        original = Some(result);
        let (a, b) = tokio::join!(
            local.post_shoutbox_idempotent(secret, ShoutboxRequestId([2; 16]), &input),
            local.post_shoutbox_idempotent(secret, ShoutboxRequestId([3; 16]), &input),
        );
        assert_ne!(a?.event_id, b?.event_id);
        let bounds = ShoutboxBounds {
            rows: 1,
            raw_rows: 256,
            payload_bytes: 65536,
        };
        if let Some(cursor) = cursor.take() {
            assert!(matches!(
                local.db.paginate_shoutbox(Some(cursor), bounds).await,
                Err(DbError::ShoutboxCursorStale)
            ));
        }
        cursor = local.db.paginate_shoutbox(None, bounds).await?.next_cursor;
        assert!(matches!(
            local
                .post_shoutbox_idempotent(RostraIdSecretKey::generate(), id, &input)
                .await,
            Err(PostError::Storage {
                source: DbError::ShoutboxCandidate,
                ..
            })
        ));
        assert!(matches!(
            local
                .post_shoutbox_idempotent(
                    secret,
                    id,
                    &ShoutboxRequest {
                        body: " changed ".into()
                    }
                )
                .await,
            Err(PostError::Storage {
                source: DbError::ShoutboxRequestMismatch,
                ..
            })
        ));
        let mut shutdown = local.clone().begin_shutdown().unwrap();
        assert!(
            local
                .post_shoutbox_idempotent(secret, id, &input)
                .await
                .is_err()
        );
        drop(local);
        shutdown.join().await?;
    }
    endpoint.close().await;
    Ok(())
}
