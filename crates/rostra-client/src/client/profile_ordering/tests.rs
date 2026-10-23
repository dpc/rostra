use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rostra_client_db::Database;
use rostra_core::Timestamp;
use rostra_core::event::content_kind::{EventContentKind as _, SocialProfileUpdate};
use rostra_core::event::{Event, EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;
use tokio::sync::Semaphore;

use crate::Client;
use crate::client::follow_ordering::FollowClock;
use crate::error::PostError;

/// Deterministic wall clock with explicitly released waits.
struct Clock {
    /// Seconds sampled by publication.
    now: AtomicU64,
    /// Whether waits advance automatically.
    automatic: bool,
    /// Announces entry into the clock wait.
    entered: Semaphore,
    /// Releases one clock wait.
    wake: Semaphore,
}

impl Clock {
    fn new(now: u64, automatic: bool) -> Self {
        Self {
            now: AtomicU64::new(now),
            automatic,
            entered: Semaphore::new(0),
            wake: Semaphore::new(0),
        }
    }
}

impl FollowClock for Clock {
    fn now(&self) -> Timestamp {
        self.now.load(Ordering::SeqCst).into()
    }

    async fn wait_next_second(&self) {
        self.entered.add_permits(1);
        if self.automatic {
            self.now.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
        } else {
            self.wake.acquire().await.unwrap().forget();
        }
    }
}

fn profile(name: &str, avatar: Option<(String, Vec<u8>)>) -> SocialProfileUpdate {
    SocialProfileUpdate {
        display_name: name.into(),
        bio: format!("{name} bio"),
        avatar,
    }
}

async fn endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap()
}

async fn client(db: Database, endpoint: &iroh::Endpoint, secret: RostraIdSecretKey) -> Arc<Client> {
    Client::builder(secret.id())
        .db(db)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await
        .unwrap()
}

async fn publish(
    client: &Client,
    secret: RostraIdSecretKey,
    profile: SocialProfileUpdate,
    clock: &Clock,
) -> VerifiedEventContent {
    let raw = profile.serialize_cbor().unwrap();
    let event = client
        .publish_event_with_clock(secret, profile, None, clock)
        .await
        .unwrap();
    VerifiedEventContent::verify(event, raw).unwrap()
}

fn imported(secret: RostraIdSecretKey, timestamp: Timestamp) -> VerifiedEventContent {
    let profile = profile("remote", None);
    let raw = profile.serialize_cbor().unwrap();
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .kind(SocialProfileUpdate::KIND)
        .maybe_singleton_aux_key(profile.singleton_key_aux())
        .content(&raw)
        .build();
    event.timestamp = timestamp.into();
    let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap();
    VerifiedEventContent::verify(event, raw).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn successive_snapshots_survive_restart_and_remote_reordering() {
    let secret = RostraIdSecretKey::generate();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profile.redb");
    let endpoint = endpoint().await;
    let clock = Clock::new(10_000, true);
    let local = client(
        Database::open(&path, secret.id()).await.unwrap(),
        &endpoint,
        secret,
    )
    .await;
    // Historical image formats remain valid signed snapshots.
    let legacy = Some(("image/svg+xml".into(), b"<svg/>".to_vec()));
    let first = publish(&local, secret, profile("first", legacy.clone()), &clock).await;
    let second = publish(&local, secret, profile("second", legacy.clone()), &clock).await;
    assert!(first.timestamp() < second.timestamp());
    local
        .publish_event(
            secret,
            rostra_core::event::content_kind::SocialPost::new_text(
                "delete profile source payload".into(),
                None,
                Default::default(),
            ),
        )
        .replace(second.event_id().into())
        .call()
        .await
        .unwrap();
    assert_eq!(
        local.db.get_self_profile_timestamp().await.unwrap(),
        Some(second.timestamp())
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    let local = client(
        Database::open(&path, secret.id()).await.unwrap(),
        &endpoint,
        secret,
    )
    .await;
    let removed = publish(&local, secret, profile("removed", None), &clock).await;
    assert!(second.timestamp() < removed.timestamp());
    for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0]] {
        let remote = Database::new_in_memory(RostraIdSecretKey::generate().id())
            .await
            .unwrap();
        for index in order {
            remote
                .try_process_event_with_content([&first, &second, &removed][index])
                .await
                .unwrap();
        }
        let actual = remote.get_social_profile(secret.id()).await.unwrap();
        assert_eq!(actual.display_name, "removed");
        assert_eq!(actual.avatar, None);
    }
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clock_failures_and_cancellation_sign_nothing_and_release_queue() {
    let secret = RostraIdSecretKey::generate();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret,
    )
    .await;
    let clock = Arc::new(Clock::new(100, false));
    let first = publish(&local, secret, profile("first", None), &clock).await;
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            local
                .publish_event_with_clock(secret, profile("cancelled", None), None, &*clock)
                .await
        })
    };
    clock.entered.acquire().await.unwrap().forget();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert_eq!(
        local.db.get_self_current_head().await,
        Some(first.event_id().into())
    );
    clock.now.store(99, Ordering::SeqCst);
    assert!(matches!(
        local
            .publish_event_with_clock(secret, profile("rollback", None), None, &*clock)
            .await,
        Err(PostError::ProfileClockBehind { .. })
    ));
    clock.now.store(100, Ordering::SeqCst);
    assert!(matches!(
        local
            .publish_event_with_clock(secret, profile("busy", None), None, &*clock)
            .await,
        Err(PostError::ProfileOrderingBusy)
    ));
    assert_eq!(
        local.db.get_self_current_head().await,
        Some(first.event_id().into())
    );
    clock.now.store(101, Ordering::SeqCst);
    publish(&local, secret, profile("recovered", None), &clock).await;
    let remote = imported(secret, Timestamp::MAX);
    local
        .db
        .try_process_event_with_content(&remote)
        .await
        .unwrap();
    assert!(matches!(
        local
            .publish_event_with_clock(secret, profile("overflow", None), None, &*clock)
            .await,
        Err(PostError::ProfileTimestampExhausted)
    ));
    assert_eq!(
        local
            .db
            .get_social_profile(secret.id())
            .await
            .unwrap()
            .display_name,
        "remote"
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_local_saves_have_distinct_order_and_public_helper_uses_wall_time() {
    let secret = RostraIdSecretKey::generate();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret,
    )
    .await;
    let clock = Clock::new(100, true);
    let (first, second) = tokio::join!(
        publish(&local, secret, profile("first", None), &clock),
        publish(&local, secret, profile("second", None), &clock),
    );
    assert_ne!(first.timestamp(), second.timestamp());
    let winner = if first.timestamp() < second.timestamp() {
        "second"
    } else {
        "first"
    };
    assert_eq!(
        local
            .db
            .get_social_profile(secret.id())
            .await
            .unwrap()
            .display_name,
        winner
    );
    let first = local
        .post_social_profile_update(secret, "public first".into(), "".into(), None)
        .await
        .unwrap();
    let second = local
        .post_social_profile_update(secret, "public second".into(), "".into(), None)
        .await
        .unwrap();
    assert!(first.timestamp() < second.timestamp());
    assert!(second.timestamp() <= Timestamp::now());
    assert_eq!(
        local
            .db
            .get_social_profile(secret.id())
            .await
            .unwrap()
            .display_name,
        "public second"
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_import_during_clock_wait_is_rechecked_before_signing() {
    let secret = RostraIdSecretKey::generate();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret,
    )
    .await;
    let clock = Arc::new(Clock::new(100, false));
    publish(&local, secret, profile("first", None), &clock).await;
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            local
                .publish_event_with_clock(secret, profile("waiting", None), None, &*clock)
                .await
        })
    };
    clock.entered.acquire().await.unwrap().forget();
    local
        .db
        .try_process_event_with_content(&imported(secret, 105.into()))
        .await
        .unwrap();
    clock.now.store(101, Ordering::SeqCst);
    clock.wake.add_permits(1);
    assert!(matches!(
        pending.await.unwrap(),
        Err(PostError::ProfileClockBehind { .. })
    ));
    assert_eq!(
        local
            .db
            .get_social_profile(secret.id())
            .await
            .unwrap()
            .display_name,
        "remote"
    );
    clock.now.store(106, Ordering::SeqCst);
    let saved = publish(&local, secret, profile("new save", None), &clock).await;
    assert_eq!(saved.timestamp(), Timestamp::from(106));
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}
