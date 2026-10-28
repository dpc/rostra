use rostra_client_db::public_attachment_request::{
    PUBLIC_ATTACHMENT_MAX_BYTES, PublicAttachmentRequest, PublicAttachmentRequestId,
};
use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraIdSecretKey;

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn public_attachment_retry_concurrency_reopen_authority_and_close() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("attachments.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let request = PublicAttachmentRequest {
        data: vec![0, 255, 128],
    };
    let id = PublicAttachmentRequestId([1; 16]);
    let mut original = None;
    for _ in 0..2 {
        // No active secret or background tasks: match existing media authority.
        let local = Client::builder(secret.id())
            .db(Database::open(&path, secret.id()).await?)
            .iroh_endpoint(endpoint.clone())
            .start_background_tasks(false)
            .start_request_handler(false)
            .build()
            .await?;
        let (a, b) = tokio::join!(
            local.publish_public_attachment_idempotent(secret, id, &request),
            local.publish_public_attachment_idempotent(secret, id, &request),
        );
        let event = a?;
        assert_eq!(event.event_id, b?.event_id);
        if let Some(original) = original {
            assert_eq!(original, event.event_id);
        }
        original = Some(event.event_id);
        #[cfg(feature = "native-media")]
        {
            use rostra_client_db::media_request::{MediaRequest, MediaRequestId};
            let normalized = rostra_core::static_media::normalize_static_media(
                "image/webp",
                include_bytes!("../../../rostra-core/src/avatar/fixtures/red-3x2.webp"),
            )?;
            let image = MediaRequest {
                mime: "image/png".to_owned(),
                data: normalized.data,
                policy_version: 1,
            };
            let image_event = local
                .publish_media_idempotent(secret, MediaRequestId(id.0), &image)
                .await?;
            assert_ne!(image_event.event_id, event.event_id);
            assert_eq!(
                local
                    .publish_public_attachment_idempotent(secret, id, &request)
                    .await?
                    .event_id,
                event.event_id
            );
        }
        let (a, b) = tokio::join!(
            local.publish_public_attachment_idempotent(
                secret,
                PublicAttachmentRequestId([2; 16]),
                &request
            ),
            local.publish_public_attachment_idempotent(
                secret,
                PublicAttachmentRequestId([3; 16]),
                &request
            ),
        );
        assert_ne!(a?.event_id, b?.event_id);
        for changed in [
            PublicAttachmentRequest { data: vec![] },
            PublicAttachmentRequest {
                data: vec![0; PUBLIC_ATTACHMENT_MAX_BYTES + 1],
            },
            PublicAttachmentRequest {
                data: vec![1, 255, 128],
            },
        ] {
            assert!(matches!(
                local
                    .publish_public_attachment_idempotent(secret, id, &changed)
                    .await,
                Err(PostError::Storage {
                    source: DbError::PublicAttachmentRequestMismatch,
                    ..
                })
            ));
        }
        for invalid in [vec![], vec![0; PUBLIC_ATTACHMENT_MAX_BYTES + 1]] {
            assert!(matches!(
                local
                    .publish_public_attachment_idempotent(
                        secret,
                        PublicAttachmentRequestId([4; 16]),
                        &PublicAttachmentRequest { data: invalid },
                    )
                    .await,
                Err(PostError::Storage {
                    source: DbError::PublicAttachmentInput,
                    ..
                })
            ));
        }
        assert!(matches!(
            local
                .publish_public_attachment_idempotent(RostraIdSecretKey::generate(), id, &request,)
                .await,
            Err(PostError::Storage {
                source: DbError::PublicAttachmentCandidate,
                ..
            })
        ));
        let mut shutdown = local.clone().begin_shutdown().unwrap();
        assert!(matches!(
            local
                .publish_public_attachment_idempotent(secret, id, &request)
                .await,
            Err(PostError::ClientClosing)
        ));
        drop(local);
        shutdown.join().await?;
    }
    endpoint.close().await;
    Ok(())
}
