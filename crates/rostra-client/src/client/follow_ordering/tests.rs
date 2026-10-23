use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicU64, AtomicUsize};

use rostra_client_db::Database;
use rostra_core::Timestamp;
use rostra_core::event::content_kind::{EventContentKind, Follow, SocialPost};
use rostra_core::event::{
    Event, EventExt as _, EventKind, PersonaTag, PersonasTagsSelector, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use tokio::sync::Semaphore;

use super::{FollowClock, WAIT_BUDGET};
use crate::Client;
use crate::error::PostError;

/// A second advances only when a test permits the publication waiter to wake.
struct Clock {
    /// Wall-clock seconds, independent of runtime timer deadlines.
    now: AtomicU64,
    /// Number of attempts to wait for a new second.
    waits: AtomicUsize,
    /// Announces that the publication is waiting, avoiding timing races.
    entered: Semaphore,
    /// Allows controlled wakeups, including without a clock advance.
    wake: Semaphore,
    /// Automatically advances one second on every requested wait.
    automatic: bool,
}

impl Clock {
    fn new(now: u64, automatic: bool) -> Self {
        Self {
            now: AtomicU64::new(now),
            waits: AtomicUsize::new(0),
            entered: Semaphore::new(0),
            wake: Semaphore::new(0),
            automatic,
        }
    }

    async fn waiting(&self) {
        self.entered.acquire().await.unwrap().forget();
    }

    fn set_and_wake(&self, now: u64) {
        self.now.store(now, SeqCst);
        self.wake.add_permits(1);
    }
}

impl FollowClock for Clock {
    fn now(&self) -> Timestamp {
        self.now.load(SeqCst).into()
    }

    async fn wait_next_second(&self) {
        self.waits.fetch_add(1, SeqCst);
        self.entered.add_permits(1);
        if self.automatic {
            self.now.fetch_add(1, SeqCst);
            tokio::task::yield_now().await;
        } else {
            self.wake.acquire().await.unwrap().forget();
        }
    }
}

fn follow(followee: RostraId, tag: Option<&str>) -> Follow {
    Follow {
        followee,
        persona: None,
        selector: None,
        persona_tags_selector: tag.map(|tag| PersonasTagsSelector::Only {
            ids: BTreeSet::from([PersonaTag::new(tag).unwrap()]),
        }),
    }
}

async fn client(db: Database, endpoint: &iroh::Endpoint, id: RostraId) -> Arc<Client> {
    Client::builder(id)
        .db(db)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await
        .unwrap()
}

async fn endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap()
}

async fn publish(
    client: &Client,
    secret: RostraIdSecretKey,
    content: Follow,
    clock: &Clock,
) -> VerifiedEventContent {
    let raw = content.serialize_cbor().unwrap();
    let event = client
        .publish_event_with_clock(secret, content, None, clock)
        .await
        .unwrap();
    VerifiedEventContent::verify(event, raw).unwrap()
}

fn imported<C: EventContentKind>(
    secret: RostraIdSecretKey,
    content: C,
    timestamp: u64,
    kind: EventKind,
) -> VerifiedEventContent {
    let raw = content.serialize_cbor().unwrap();
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .kind(kind)
        .maybe_singleton_aux_key(content.singleton_key_aux())
        .content(&raw)
        .build();
    event.timestamp = timestamp.into();
    let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap();
    VerifiedEventContent::verify(event, raw).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn follow_selector_unfollow_and_refollow_survive_restart_and_remote_reordering() {
    let secret = RostraIdSecretKey::from_bytes([71; 32]);
    let peer = RostraIdSecretKey::from_bytes([72; 32]).id();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("client.redb");
    let endpoint = endpoint().await;
    let clock = Clock::new(10_000, true);
    let local = client(
        Database::open(&path, secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let first = publish(&local, secret, follow(peer, Some("personal")), &clock).await;
    let selected = publish(&local, secret, follow(peer, Some("professional")), &clock).await;
    assert_eq!(first.timestamp(), Timestamp::from(10_000));
    assert_eq!(selected.timestamp(), Timestamp::from(10_001));
    let snapshot = local.db.get_self_followees_snapshot().await.unwrap();
    assert_eq!(snapshot[0].first_ts, first.timestamp());
    assert_eq!(
        snapshot[0].persona_selector,
        follow(peer, Some("professional"))
            .persona_tags_selector
            .unwrap()
    );
    for n in 0..43 {
        local
            .social_post(secret, format!("unrelated post {n}"), None, BTreeSet::new())
            .await
            .unwrap();
    }
    assert_eq!(clock.now(), selected.timestamp());
    local.begin_shutdown().unwrap().join().await.unwrap();
    let local = client(
        Database::open(&path, secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let removed = publish(&local, secret, follow(peer, None), &clock).await;
    assert_eq!(removed.timestamp(), Timestamp::from(10_002));
    assert!(
        local
            .db
            .get_self_followees_snapshot()
            .await
            .unwrap()
            .is_empty()
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    let local = client(
        Database::open(&path, secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    assert_eq!(
        local.db.get_self_follow_timestamp(peer).await.unwrap(),
        Some(removed.timestamp())
    );
    let restored = publish(&local, secret, follow(peer, Some("custom")), &clock).await;
    assert_eq!(restored.timestamp(), Timestamp::from(10_003));
    let expected = local.db.get_self_followees_snapshot().await.unwrap();
    assert_eq!(expected[0].first_ts, restored.timestamp());
    let events = [first, selected, removed, restored];
    for order in [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1]] {
        let remote = Database::new_in_memory(secret.id()).await.unwrap();
        for index in order {
            remote
                .try_process_event_with_content(&events[index])
                .await
                .unwrap();
        }
        assert_eq!(
            remote.get_self_followees_snapshot().await.unwrap(),
            expected
        );
        // The last second before the new epoch remains historical, while its
        // first second still produces a fresh notification.
        for timestamp in [10_002, 10_003] {
            let post = imported(
                RostraIdSecretKey::from_bytes([72; 32]),
                SocialPost::new(format!("peer post {timestamp}"), None, BTreeSet::new()),
                timestamp,
                EventKind::SOCIAL_POST,
            );
            let received_after = Timestamp::now();
            remote.try_process_event_with_content(&post).await.unwrap();
            let cursor = remote
                .get_latest_social_post_received_at_cursor()
                .await
                .unwrap();
            if timestamp == 10_002 {
                assert_eq!(cursor.ts, Timestamp::from(timestamp));
            } else {
                assert!(received_after <= cursor.ts);
            }
        }
    }
    assert_eq!(clock.waits.load(SeqCst), 3);
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_local_mutations_serialize_but_other_followees_do_not_wait() {
    let secret = RostraIdSecretKey::generate();
    let peer = RostraIdSecretKey::generate().id();
    let other = RostraIdSecretKey::generate().id();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let clock = Arc::new(Clock::new(100, false));
    publish(&local, secret, follow(peer, Some("personal")), &clock).await;
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            publish(&local, secret, follow(peer, Some("professional")), &clock).await
        })
    };
    clock.waiting().await;
    let other_event = publish(&local, secret, follow(other, Some("personal")), &clock).await;
    assert_eq!(other_event.timestamp(), Timestamp::from(100));
    let queued = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move { publish(&local, secret, follow(peer, None), &clock).await })
    };
    clock.set_and_wake(101);
    let selected = pending.await.unwrap();
    clock.waiting().await;
    clock.set_and_wake(102);
    let removed = queued.await.unwrap();
    assert_eq!(selected.timestamp(), Timestamp::from(101));
    assert_eq!(removed.timestamp(), Timestamp::from(102));
    assert_eq!(
        local.db.get_self_followees_snapshot().await.unwrap().len(),
        1
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn future_legacy_tombstone_and_exhaustion_fail_without_mutation() {
    let secret = RostraIdSecretKey::generate();
    let peer = RostraIdSecretKey::generate().id();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let clock = Clock::new(100, true);
    for (timestamp, exhausted) in [(200, false), (u64::MAX, true)] {
        let imported = imported(secret, follow(peer, None), timestamp, EventKind::UNFOLLOW);
        local
            .db
            .try_process_event_with_content(&imported)
            .await
            .unwrap();
        let head = local.db.get_self_current_head().await;
        let error = local
            .publish_event_with_clock(secret, follow(peer, Some("personal")), None, &clock)
            .await
            .unwrap_err();
        if exhausted {
            assert!(matches!(error, PostError::FollowTimestampExhausted));
        } else {
            assert!(
                matches!(error, PostError::FollowClockBehind { latest, now } if latest == Timestamp::from(200) && now == Timestamp::from(100))
            );
        }
        assert_eq!(local.db.get_self_current_head().await, head);
        assert!(
            local
                .db
                .get_self_followees_snapshot()
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(clock.waits.load(SeqCst), 0);
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clock_wait_cancellation_and_shutdown_do_not_publish() {
    let secret = RostraIdSecretKey::generate();
    let peer = RostraIdSecretKey::generate().id();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let clock = Arc::new(Clock::new(100, false));
    let first = publish(&local, secret, follow(peer, Some("personal")), &clock).await;
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            local
                .publish_event_with_clock(secret, follow(peer, None), None, &*clock)
                .await
        })
    };
    clock.waiting().await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert_eq!(
        local.db.get_self_follow_timestamp(peer).await.unwrap(),
        Some(first.timestamp())
    );
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            local
                .publish_event_with_clock(secret, follow(peer, None), None, &*clock)
                .await
        })
    };
    clock.waiting().await;
    let mut closing = local.clone().begin_shutdown().unwrap();
    clock.set_and_wake(101);
    assert!(matches!(
        pending.await.unwrap(),
        Err(PostError::ClientClosing)
    ));
    assert_eq!(
        local.db.get_self_current_head().await,
        Some(first.event_id().into())
    );
    drop(local);
    closing.join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clock_and_queue_deadlines_return_retryable_error_without_publication() {
    let secret = RostraIdSecretKey::generate();
    let peer = RostraIdSecretKey::generate().id();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let clock = Arc::new(Clock::new(100, false));
    let first = publish(&local, secret, follow(peer, Some("personal")), &clock).await;
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            local
                .publish_event_with_clock(secret, follow(peer, None), None, &*clock)
                .await
        })
    };
    clock.waiting().await;
    let queued = local.publish_event_with_clock(secret, follow(peer, None), None, &*clock);
    let (pending, queued) =
        tokio::time::timeout(WAIT_BUDGET * 3, async { tokio::join!(pending, queued) })
            .await
            .unwrap();
    assert!(matches!(
        pending.unwrap(),
        Err(PostError::FollowOrderingBusy)
    ));
    assert!(matches!(queued, Err(PostError::FollowOrderingBusy)));
    assert_eq!(
        local.db.get_self_current_head().await,
        Some(first.event_id().into())
    );
    clock.set_and_wake(101);
    publish(&local, secret, follow(peer, None), &clock).await;
    assert!(
        local
            .db
            .get_self_followees_snapshot()
            .await
            .unwrap()
            .is_empty()
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_during_wait_fails_without_signing() {
    let secret = RostraIdSecretKey::generate();
    let peer = RostraIdSecretKey::generate().id();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let clock = Arc::new(Clock::new(100, false));
    let first = publish(&local, secret, follow(peer, Some("personal")), &clock).await;
    let pending = {
        let local = local.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            local
                .publish_event_with_clock(secret, follow(peer, None), None, &*clock)
                .await
        })
    };
    clock.waiting().await;
    clock.set_and_wake(99);
    assert!(matches!(
        pending.await.unwrap(),
        Err(PostError::FollowClockBehind { .. })
    ));
    assert_eq!(
        local.db.get_self_current_head().await,
        Some(first.event_id().into())
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn public_follow_and_unfollow_use_wall_clock_policy() {
    let secret = RostraIdSecretKey::generate();
    let peer = RostraIdSecretKey::generate().id();
    let endpoint = endpoint().await;
    let local = client(
        Database::new_in_memory(secret.id()).await.unwrap(),
        &endpoint,
        secret.id(),
    )
    .await;
    let first = local
        .follow(secret, peer, PersonasTagsSelector::default())
        .await
        .unwrap();
    let removed = local.unfollow(secret, peer).await.unwrap();
    assert!(first.timestamp() < removed.timestamp());
    assert!(removed.timestamp() <= Timestamp::now());
    assert!(
        local
            .db
            .get_self_followees_snapshot()
            .await
            .unwrap()
            .is_empty()
    );
    local.begin_shutdown().unwrap().join().await.unwrap();
    endpoint.close().await;
}
