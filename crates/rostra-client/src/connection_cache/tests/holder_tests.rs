use std::num::{NonZeroU64, NonZeroUsize};

use rostra_core::event::EventSignature;

use super::*;

async fn observe(
    local: &Client,
    cache: &ConnectionCache,
    peer: rostra_core::id::RostraId,
    event: &rostra_core::event::VerifiedEvent,
) {
    local.db().try_process_event(event).await.unwrap();
    let (_, node) = local
        .db()
        .get_id_endpoints(peer)
        .await
        .into_keys()
        .next()
        .unwrap();
    let endpoint = iroh::EndpointId::from_bytes(&node.to_bytes()).unwrap();
    cache
        .holders
        .observe_retained(local.db(), event, endpoint)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn holder_wrong_author_id_signature_and_payload_are_rejected_with_fallback() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 71);
    let mut bad_signature: SignedEvent = wanted.into();
    bad_signature.sig = EventSignature::from_bytes([0; 64]);
    for bad in [
        SignedEvent::from(test_event(RostraIdSecretKey::generate(), 71)),
        SignedEvent::from(test_event(author, 72)),
        bad_signature,
    ] {
        let (local, peers, _, _, servers) = make_test_network(vec![
            PeerBehavior::Event(bad),
            PeerBehavior::Event(wanted.into()),
        ])
        .await;
        let cache = ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE);
        observe(&local, &cache, peers[0], &wanted).await;
        assert!(
            cache
                .get_event_from_peers(
                    local.networking(),
                    &[],
                    author.id(),
                    wanted.event_id.to_short()
                )
                .await
                .is_none()
        );
        let result = cache
            .get_event_from_peers(
                local.networking(),
                &peers[1..],
                author.id(),
                wanted.event_id.to_short(),
            )
            .await
            .unwrap();
        assert_eq!(result.event_id, wanted.event_id);
        abort_servers(servers).await;
    }

    let (local, peers, _, _, servers) = make_test_network(vec![
        PeerBehavior::Content(EventContentRaw::new(vec![72])),
        PeerBehavior::Content(EventContentRaw::new(vec![71])),
    ])
    .await;
    let cache = ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE);
    observe(&local, &cache, peers[0], &wanted).await;
    assert!(
        !cache
            .fetch_event_content_from_peers(local.networking(), &[], wanted, local.db())
            .await
            .unwrap()
    );
    assert!(
        local
            .db()
            .get_event_content(wanted.event_id)
            .await
            .is_none()
    );
    assert!(
        cache
            .fetch_event_content_from_peers(local.networking(), &peers[1..], wanted, local.db())
            .await
            .unwrap()
    );
    assert_eq!(
        local
            .db()
            .get_event_content(wanted.event_id)
            .await
            .unwrap()
            .as_ref(),
        &[71]
    );
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn later_honest_holder_wins_after_failure_without_replacing_account_cache() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 73);
    let (local, peers, _, _, servers) = make_test_network(vec![
        PeerBehavior::Missing,
        PeerBehavior::Event(wanted.into()),
    ])
    .await;
    let cache = ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE);
    let original = cache
        .get_or_connect(local.networking(), peers[0])
        .await
        .unwrap();
    observe(&local, &cache, peers[0], &wanted).await;
    assert!(
        cache
            .get_event_from_peers(
                local.networking(),
                &[],
                author.id(),
                wanted.event_id.to_short()
            )
            .await
            .is_none()
    );
    observe(&local, &cache, peers[1], &wanted).await;
    let result = cache
        .get_event_from_peers(
            local.networking(),
            &[],
            author.id(),
            wanted.event_id.to_short(),
        )
        .await
        .unwrap();
    assert_eq!(result.event_id, wanted.event_id);
    assert_eq!(
        cache
            .get_or_connect(local.networking(), peers[0])
            .await
            .unwrap()
            .remote_id(),
        original.remote_id()
    );
    assert!(local.db().get_id_endpoints(author.id()).await.is_empty());
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_holder_does_not_delay_successful_ordinary_route() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 74);
    let (local, peers, _, _, servers) = make_test_network(vec![
        PeerBehavior::HangEvent,
        PeerBehavior::Event(wanted.into()),
    ])
    .await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[0], &wanted).await;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        cache.get_event_from_peers(
            local.networking(),
            &peers[1..],
            author.id(),
            wanted.event_id.to_short(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.event_id, wanted.event_id);
    assert_eq!(cache.holders.operations.available_permits(), 4);
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn global_holder_limit_and_shutdown_release_all_owned_operations() {
    let author = RostraIdSecretKey::generate();
    let (local, peers, notifications, _, servers) =
        make_test_network(vec![PeerBehavior::HangEvent]).await;
    let cache = local.connection_cache().clone();
    for marker in 0..4 {
        let event = test_event(author, marker);
        observe(&local, &cache, peers[0], &event).await;
        let networking = local.networking().clone();
        let cache = cache.clone();
        local.task_handles.spawn(async move {
            cache
                .get_event_from_peers(&networking, &[], author.id(), event.event_id.to_short())
                .await;
        });
        tokio::time::timeout(Duration::from_secs(2), notifications[0].notified())
            .await
            .unwrap();
    }
    assert_eq!(cache.holders.operations.available_permits(), 0);
    let extra = test_event(author, 90);
    observe(&local, &cache, peers[0], &extra).await;
    assert!(
        cache
            .get_event_from_peers(
                local.networking(),
                &[],
                author.id(),
                extra.event_id.to_short()
            )
            .await
            .is_none()
    );
    let mut shutdown = local.begin_shutdown().unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cache.holders.operations.available_permits(), 4);
    abort_servers(servers).await;
}

async fn one_read_db(id: rostra_core::id::RostraId) -> Database {
    use rostra_client_db::{
        PayloadAccount, PayloadAdmissionConfig, PayloadAdmissionLimits, PayloadRetentionConfig,
        PayloadRuntimeLimits,
    };
    let account = PayloadAccount::configured(
        id,
        PayloadRetentionConfig::Enforce {
            policy: rostra_core::retention::RetentionPolicy::new(1, 1, 0, 0, 1, 0).unwrap(),
            admission: PayloadAdmissionConfig::new(PayloadAdmissionLimits {
                database_bytes: NonZeroU64::new(1000).unwrap(),
                author_bytes: NonZeroU64::new(1000).unwrap(),
                overrides: Default::default(),
                in_flight_count: NonZeroUsize::new(2).unwrap(),
                in_flight_bytes: NonZeroU64::new(2).unwrap(),
            })
            .unwrap(),
            limits: PayloadRuntimeLimits {
                operations: NonZeroUsize::new(32).unwrap(),
                bytes: 1000,
                gc_bytes: 1000,
                time: Duration::from_millis(20),
            },
        },
    )
    .unwrap();
    let mut db = Database::new_in_memory(id).await.unwrap();
    db.attach_payload_account(&account).unwrap();
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await
        .unwrap()
        .ready
    {}
    db
}

#[tokio::test(flavor = "multi_thread")]
async fn scarce_buffers_preserve_ordinary_fallback_and_then_allow_holder_content() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 91);
    let gate = Arc::new(Notify::new());
    let (local, peers, _, content_requests, servers) = make_test_network(vec![
        PeerBehavior::Missing,
        PeerBehavior::GatedContent(EventContentRaw::new(vec![91]), gate.clone()),
        PeerBehavior::HangContent,
    ])
    .await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[2], &wanted).await;
    let db = one_read_db(local.rostra_id()).await;
    let fetch = cache.fetch_event_content_from_peers(local.networking(), &peers[..2], wanted, &db);
    tokio::pin!(fetch);
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = content_requests[1].notified() => {},
            result = &mut fetch => panic!("ordinary gated read must own buffers: {result:?}"),
        }
    })
    .await
    .unwrap();
    assert_eq!(db.payload_admission_usage().buffers, 2);
    assert_eq!(db.payload_admission_usage().buffer_bytes, 2);
    assert_eq!(cache.holders.operations.available_permits(), 4);
    gate.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut fetch)
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(db.payload_admission_usage().buffers, 0);
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    abort_servers(servers).await;

    // Ordinary absence must not suppress the holder when only one read fits.
    let wanted = test_event(author, 92);
    let (local, peers, _, _, servers) = make_test_network(vec![
        PeerBehavior::Missing,
        PeerBehavior::Content(EventContentRaw::new(vec![92])),
    ])
    .await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[1], &wanted).await;
    let db = one_read_db(local.rostra_id()).await;
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            cache.fetch_event_content_from_peers(local.networking(), &peers[..1], wanted, &db),
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        db.get_event_content(wanted.event_id)
            .await
            .unwrap()
            .as_ref(),
        &[92]
    );
    assert_eq!(db.payload_admission_usage().buffers, 0);
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_holder_content_releases_real_admission_buffers() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 93);
    let (local, peers, _, content_requests, servers) =
        make_test_network(vec![PeerBehavior::HangContent]).await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[0], &wanted).await;
    let db = one_read_db(local.rostra_id()).await;
    {
        let fetch = cache.fetch_event_content_from_peers(local.networking(), &[], wanted, &db);
        tokio::pin!(fetch);
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = content_requests[0].notified() => {},
                result = &mut fetch => panic!("holder must block with buffers: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(db.payload_admission_usage().buffers, 2);
        assert_eq!(cache.holders.operations.available_permits(), 3);
    }
    assert_eq!(db.payload_admission_usage().buffers, 0);
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(cache.holders.operations.available_permits(), 4);
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hanging_holder_obeys_five_second_complete_operation_limit() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 94);
    let (local, peers, _, _, servers) = make_test_network(vec![PeerBehavior::HangEvent]).await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[0], &wanted).await;
    let started = tokio::time::Instant::now();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(10),
            cache.get_event_from_peers(
                local.networking(),
                &[],
                author.id(),
                wanted.event_id.to_short()
            ),
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert_eq!(cache.holders.operations.available_permits(), 4);
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn holder_occupies_existing_race_slot_instead_of_adding_fanout() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 95);
    let (local, peers, requests, _, servers) =
        make_test_network(vec![PeerBehavior::HangEvent; 5]).await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[0], &wanted).await;
    {
        let fetch = cache.get_event_from_peers(
            local.networking(),
            &peers[1..],
            author.id(),
            wanted.event_id.to_short(),
        );
        tokio::pin!(fetch);
        for request in &requests[..4] {
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::select! {
                    () = request.notified() => {},
                    _ = &mut fetch => panic!("stalled reads should remain pending"),
                }
            })
            .await
            .unwrap();
        }
        tokio::select! {
            () = requests[4].notified() => panic!("holder must not create a fifth active read"),
            _ = &mut fetch => panic!("stalled reads should remain pending"),
            () = tokio::time::sleep(Duration::from_millis(30)) => {},
        }
        assert_eq!(cache.holders.operations.available_permits(), 3);
    }
    assert_eq!(cache.holders.operations.available_permits(), 4);
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn buffer_admission_refusal_does_not_start_holder_and_retry_recovers() {
    let author = RostraIdSecretKey::generate();
    let wanted = test_event(author, 96);
    let (local, peers, _, requests, servers) =
        make_test_network(vec![PeerBehavior::Content(EventContentRaw::new(vec![96]))]).await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[0], &wanted).await;
    let db = one_read_db(local.rostra_id()).await;
    let occupied = db.reserve_payload_allocation(1).unwrap().unwrap();
    assert!(matches!(
        cache
            .fetch_event_content_from_peers(local.networking(), &[], wanted, &db)
            .await,
        Err(rostra_client_db::DbError::PayloadAdmissionPaused { .. })
    ));
    assert_eq!(db.payload_admission_usage().buffers, 1);
    assert_eq!(cache.holders.operations.available_permits(), 4);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), requests[0].notified())
            .await
            .is_err()
    );
    drop(occupied);
    assert!(
        cache
            .fetch_event_content_from_peers(local.networking(), &[], wanted, &db)
            .await
            .unwrap()
    );
    assert_eq!(db.payload_admission_usage().buffers, 0);
    abort_servers(servers).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_header_winner_preserves_observed_route_to_missing_grandparent() {
    let author = RostraIdSecretKey::generate();
    let make = |parent| {
        rostra_core::event::VerifiedEvent::verify_received_as_is(
            Event::builder_raw_content()
                .author(author.id())
                .kind(EventKind::NULL)
                .content(&EventContentRaw::new(vec![]))
                .maybe_parent_prev(parent)
                .build()
                .signed_by(author),
        )
        .unwrap()
    };
    let grandparent = make(None);
    let parent = make(Some(grandparent.event_id.to_short()));
    let head = make(Some(parent.event_id.to_short()));
    let (local, peers, _, _, servers) = make_test_network(vec![
        PeerBehavior::Event(grandparent.into()),
        PeerBehavior::Event(parent.into()),
    ])
    .await;
    let cache = ConnectionCache::new();
    observe(&local, &cache, peers[0], &head).await;
    assert!(local.db().get_event(parent.event_id).await.is_none());
    let held = cache.holders.operations.acquire_many(4).await.unwrap();
    assert!(
        crate::util::rpc::download_events_from_child(
            author.id(),
            parent.event_id.to_short(),
            local.networking(),
            &cache,
            &peers[1..],
            local.db(),
        )
        .await
        .unwrap()
    );
    assert!(local.db().get_event(parent.event_id).await.is_some());
    assert!(local.db().get_event(grandparent.event_id).await.is_none());
    // No holder read could win while all permits were held. Nevertheless the
    // verified ordinary response extended the already-observed provenance.
    cache
        .holders
        .attempt(author.id(), grandparent.event_id.to_short())
        .unwrap()
        .succeeded();
    drop(held);
    assert!(
        crate::util::rpc::download_events_from_child(
            author.id(),
            grandparent.event_id.to_short(),
            local.networking(),
            &cache,
            &peers[1..],
            local.db(),
        )
        .await
        .unwrap()
    );
    assert!(local.db().get_event(grandparent.event_id).await.is_some());
    assert!(local.db().get_id_endpoints(author.id()).await.is_empty());
    abort_servers(servers).await;
}
