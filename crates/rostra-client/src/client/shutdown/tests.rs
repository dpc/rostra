use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt as _;
use rostra_client_db::Database;
use rostra_core::id::RostraIdSecretKey;
use tokio::sync::Notify;

use super::ClientShutdownError;
use crate::Client;
use crate::error::{ActivateError, PostError};

/// Observes destruction even when a submitted future was never polled.
struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap()
}

async fn local_client(
    id: rostra_core::id::RostraId,
    db: Database,
    endpoint: iroh::Endpoint,
) -> Arc<Client> {
    Client::builder(id)
        .db(db)
        .iroh_endpoint(endpoint)
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_retries_external_owners_and_reopens_without_runtime_teardown() {
    let secret = RostraIdSecretKey::generate();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("client.redb");
    let endpoint = endpoint().await;
    let client = local_client(
        secret.id(),
        Database::open(&path, secret.id()).await.unwrap(),
        endpoint.clone(),
    )
    .await;
    let posted = client
        .social_post(secret, "before close".into(), None, BTreeSet::new())
        .await
        .unwrap();
    let weak = Arc::downgrade(&client);
    let handle = client.handle();
    let external = client.clone();
    let database = client.db().clone();
    let mut closing = client.begin_shutdown().unwrap();

    assert!(handle.client_ref().is_err());
    assert!(handle.db().is_err());
    assert!(matches!(
        external.clone().begin_shutdown(),
        Err(ClientShutdownError::AlreadyClosing)
    ));
    assert!(matches!(
        external.unlock_active(secret).await,
        Err(ActivateError::ActivationClosing)
    ));
    assert!(matches!(
        external
            .social_post(secret, "too late".into(), None, BTreeSet::new())
            .await,
        Err(PostError::ClientClosing)
    ));
    assert!(matches!(
        external.publish_omni_tbd(secret, "too late".into()).await,
        Err(PostError::ClientClosing)
    ));
    assert!(matches!(
        external.maintain_direct_messages(secret).await,
        Err(PostError::ClientClosing)
    ));
    assert!(matches!(
        closing.join().await,
        Err(ClientShutdownError::OutstandingClientOwners)
    ));
    drop(external);
    assert!(matches!(
        closing.join().await,
        Err(ClientShutdownError::OutstandingDatabaseOwners)
    ));
    assert!(weak.upgrade().is_none());
    assert!(database.has_event(posted.event_id).await);
    drop(database);
    closing.join().await.unwrap();
    closing.join().await.unwrap();
    assert!(
        !endpoint.is_closed(),
        "injected transport remains caller-owned"
    );

    let reopened = Database::open(&path, secret.id()).await.unwrap();
    assert!(reopened.has_event(posted.event_id).await);
    let reopened = local_client(secret.id(), reopened, endpoint.clone()).await;
    reopened.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_join_retains_descendants_and_indivisible_database_work() {
    let secret = RostraIdSecretKey::generate();
    let client = local_client(
        secret.id(),
        Database::new_in_memory(secret.id()).await.unwrap(),
        endpoint().await,
    )
    .await;
    let db = client.db().clone();
    let weak_db = Arc::downgrade(&db);
    let tasks = Arc::clone(&client.task_handles);
    let parent_tasks = tasks.clone();
    let started = Arc::new(Notify::new());
    let finished = Arc::new(AtomicBool::new(false));
    let task_started = started.clone();
    let task_finished = finished.clone();
    let (release, blocked) = std::sync::mpsc::channel();
    tasks.spawn(async move {
        let child = parent_tasks
            .spawn_child(async move {
                db.extension_write(move |_| {
                    task_started.notify_one();
                    let _ = blocked.recv();
                    task_finished.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap();
            })
            .unwrap();
        // The parent owns cancellation; the registry must still join this child
        // after aborting the parent has dropped that cancellation owner.
        let _ = child.await;
    });
    started.notified().await;
    let mut closing = client.begin_shutdown().unwrap();
    assert!(closing.join().now_or_never().is_none());
    assert!(closing.join().now_or_never().is_none());
    assert!(!finished.load(Ordering::SeqCst));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), closing.join())
        .await
        .unwrap()
        .unwrap();
    assert!(finished.load(Ordering::SeqCst));
    assert!(weak_db.upgrade().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_closes_owned_endpoint_and_joins_panicked_worker() {
    let id = RostraIdSecretKey::generate().id();
    let client = Client::builder(id)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .unwrap();
    let endpoint = client.networking.endpoint.clone();
    let started = Arc::new(Notify::new());
    let task_started = started.clone();
    client.task_handles.spawn(async move {
        task_started.notify_one();
        panic!("injected worker failure");
    });
    started.notified().await;
    let mut closing = client.begin_shutdown().unwrap();
    closing.join().await.unwrap();
    assert!(endpoint.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_database_destruction_never_becomes_success_on_retry() {
    let id = RostraIdSecretKey::generate().id();
    let client = local_client(
        id,
        Database::new_in_memory(id).await.unwrap(),
        endpoint().await,
    )
    .await;
    let mut closing = client.begin_shutdown().unwrap();
    closing.join().await.unwrap();
    closing.database_close = Some(tokio::task::spawn_blocking(|| {
        panic!("injected close failure")
    }));
    assert!(matches!(
        closing.join().await,
        Err(ClientShutdownError::DatabaseClose { .. })
    ));
    assert!(matches!(
        closing.join().await,
        Err(ClientShutdownError::DatabaseClose { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_join_retains_blocking_database_close() {
    let id = RostraIdSecretKey::generate().id();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("closing.redb");
    let database = Database::open(&path, id).await.unwrap();
    let started = Arc::new(Notify::new());
    let task_started = started.clone();
    let (release, blocked) = std::sync::mpsc::channel();
    // Enter the final shutdown phase with real storage and a controlled
    // blocking destructor, independently of task/endpoint wait cancellation.
    let mut closing = super::ClientShutdown {
        client: None,
        database: None,
        endpoint_close: None,
        database_close: Some(tokio::task::spawn_blocking(move || {
            task_started.notify_one();
            let _ = blocked.recv();
            drop(database);
        })),
        failure: None,
    };
    started.notified().await;
    assert!(closing.join().now_or_never().is_none());
    assert!(closing.join().now_or_never().is_none());
    release.send(()).unwrap();
    closing.join().await.unwrap();
    closing.join().await.unwrap();
    drop(Database::open(&path, id).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn task_admission_racing_shutdown_never_detaches_work() {
    for _ in 0..32 {
        let tasks = Arc::new(crate::client::tasks::ClientTasks::default());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let dropped = Arc::new(AtomicBool::new(false));
        let submit_tasks = tasks.clone();
        let submit_barrier = barrier.clone();
        let drop_signal = DropSignal(dropped.clone());
        let runtime = tokio::runtime::Handle::current();
        let submit = std::thread::spawn(move || {
            let _entered = runtime.enter();
            submit_barrier.wait();
            submit_tasks.spawn(async move {
                let _drop_signal = drop_signal;
                std::future::pending::<()>().await;
            });
        });
        barrier.wait();
        tasks.abort();
        submit.join().unwrap();
        tasks.terminated().await;
        // No additional spawn is admitted after the completion snapshot.
        assert!(tasks.spawn_child(async {}).is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }
}
