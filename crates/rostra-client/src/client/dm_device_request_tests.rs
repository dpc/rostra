use std::sync::atomic::Ordering::SeqCst;

use rostra_client_db::dm_device_request::{
    DmDeviceRequest, DmDeviceRequestId, DmEnrollmentExpectation,
};
use rostra_client_db::{Database, DbError};
use rostra_core::id::RostraIdSecretKey;

use super::Client;
use crate::error::PostError;

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_enforce_authority_before_retries_and_reopen() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("devices.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let request = DmDeviceRequest::Reenroll {
        expected: DmEnrollmentExpectation::Absent,
    };
    let id = DmDeviceRequestId([1; 16]);
    let mut accepted = None;
    for _ in 0..2 {
        let client = Client::builder(secret.id())
            .db(Database::open(&path, secret.id()).await?)
            .iroh_endpoint(endpoint.clone())
            .start_background_tasks(false)
            .start_request_handler(false)
            .build()
            .await?;
        assert!(
            client
                .mutate_direct_message_device_idempotent(secret, id, &request)
                .await
                .is_err()
        );
        // Avoid discovery/worker networking; exercise the real authority predicate.
        client.active.store(true, SeqCst);
        let outcome = client
            .mutate_direct_message_device_idempotent(secret, id, &request)
            .await?;
        if let Some(original) = accepted {
            assert_eq!(outcome.event.event_id, original);
            assert!(client.db.dm_local_installation().await?.unwrap().retired);
        } else {
            accepted = Some(outcome.event.event_id);
            client
                .mutate_direct_message_device_idempotent(
                    secret,
                    DmDeviceRequestId([2; 16]),
                    &DmDeviceRequest::Retire {
                        device_id: outcome.device_id,
                    },
                )
                .await?;
        }
        assert!(matches!(
            client
                .mutate_direct_message_device_idempotent(
                    secret,
                    id,
                    &DmDeviceRequest::Retire {
                        device_id: outcome.device_id
                    }
                )
                .await,
            Err(PostError::Storage {
                source: DbError::DmDeviceRequestMismatch,
                ..
            })
        ));
        assert!(
            client
                .mutate_direct_message_device_idempotent(
                    RostraIdSecretKey::generate(),
                    id,
                    &request
                )
                .await
                .is_err()
        );
        client.active.store(false, SeqCst);
        assert!(
            client
                .mutate_direct_message_device_idempotent(secret, id, &request)
                .await
                .is_err()
        );
        client.active.store(true, SeqCst);
        let mut shutdown = client.clone().begin_shutdown().unwrap();
        assert!(
            client
                .mutate_direct_message_device_idempotent(secret, id, &request)
                .await
                .is_err()
        );
        drop(client);
        shutdown.join().await?;
    }
    let light = Client::builder(secret.id())
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await?;
    light.active.store(true, SeqCst);
    assert!(
        light
            .mutate_direct_message_device_idempotent(secret, id, &request)
            .await
            .is_err()
    );
    let mut shutdown = light.clone().begin_shutdown().unwrap();
    drop(light);
    shutdown.join().await?;
    endpoint.close().await;
    Ok(())
}
