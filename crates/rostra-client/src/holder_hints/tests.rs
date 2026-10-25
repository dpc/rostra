use rostra_core::event::{Event, EventContentRaw, EventKind};
use rostra_core::id::RostraIdSecretKey;

use super::*;

fn event(marker: u8, parent: Option<ShortEventId>) -> VerifiedEvent {
    let key = RostraIdSecretKey::from_bytes([marker; 32]);
    VerifiedEvent::verify_received_as_is(
        Event::builder_raw_content()
            .author(key.id())
            .kind(EventKind::NULL)
            .content(&EventContentRaw::new(vec![marker]))
            .maybe_parent_prev(parent)
            .build()
            .signed_by(key),
    )
    .unwrap()
}

fn endpoint(marker: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[marker; 32]).public()
}

#[tokio::test(start_paused = true)]
async fn duplicates_preserve_expiry_and_cooldown_ancestors_inherit_deadline() {
    let hints = HolderHints::new();
    let parent = event(1, None);
    let child = event(2, Some(parent.event_id.to_short()));
    hints.observe(&child, endpoint(1));
    let first = hints
        .take(child.author(), child.event_id.to_short())
        .unwrap();
    for _ in 0..100 {
        hints.observe(&child, endpoint(1));
    }
    assert!(
        hints
            .take(child.author(), child.event_id.to_short())
            .is_none()
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let retry = hints
        .take(child.author(), child.event_id.to_short())
        .unwrap();
    assert_eq!(retry.expires, first.expires);
    hints.inherit(&parent, first);
    assert_eq!(
        hints
            .take(parent.author(), parent.event_id.to_short())
            .unwrap()
            .expires,
        first.expires
    );
    tokio::time::advance(LIFETIME).await;
    assert!(
        hints
            .take(parent.author(), parent.event_id.to_short())
            .is_none()
    );
    hints.inherit(&parent, first);
    assert!(
        hints
            .take(parent.author(), parent.event_id.to_short())
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn keys_are_author_scoped_and_new_honest_source_can_replace_poison() {
    let hints = HolderHints::new();
    let event = event(3, None);
    hints.observe(&event, endpoint(1));
    hints.observe(&event, endpoint(2));
    assert!(
        hints
            .take(
                RostraIdSecretKey::from_bytes([4; 32]).id(),
                event.event_id.to_short()
            )
            .is_none()
    );
    assert_eq!(
        hints
            .take(event.author(), event.event_id.to_short())
            .unwrap()
            .endpoint,
        endpoint(1)
    );
    hints.observe(&event, endpoint(3));
    assert_eq!(
        hints
            .take(event.author(), event.event_id.to_short())
            .unwrap()
            .endpoint,
        endpoint(2)
    );
    assert_eq!(
        hints
            .take(event.author(), event.event_id.to_short())
            .unwrap()
            .endpoint,
        endpoint(3)
    );
    assert!(
        hints
            .take(event.author(), event.event_id.to_short())
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn capacity_and_bookkeeping_remain_bounded_and_restart_has_no_routes() {
    let hints = HolderHints::new();
    let base = event(5, None);
    for number in 0..2048_u64 {
        let key = RostraIdSecretKey::from_bytes([5; 32]);
        let item = VerifiedEvent::verify_received_as_is(
            Event::builder_raw_content()
                .author(key.id())
                .kind(EventKind::NULL)
                .content(&EventContentRaw::new(number.to_le_bytes().to_vec()))
                .build()
                .signed_by(key),
        )
        .unwrap();
        hints.observe(&item, endpoint(1));
        hints.observe(&item, endpoint(2));
        hints.observe(&item, endpoint(3));
    }
    let entries = hints.entries.lock().unwrap();
    assert_eq!(entries.len(), MAX_KEYS);
    assert!(entries.values().all(|holders| holders.len() <= MAX_HOLDERS));
    drop(entries);
    assert!(
        HolderHints::new()
            .take(base.author(), base.event_id.to_short())
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unretained_or_different_full_envelope_cannot_register_or_propagate() {
    let original = event(6, None);
    let db = rostra_client_db::Database::new_in_memory(original.author())
        .await
        .unwrap();
    let hints = HolderHints::new();
    hints
        .observe_retained(&db, &original, endpoint(1))
        .await
        .unwrap();
    assert!(hints.entries.lock().unwrap().is_empty());
    db.try_process_event(&original).await.unwrap();
    let mut collision = event(7, None);
    // Simulate an infeasible short-ID collision at the trusted verified boundary.
    collision.event_id = original.event_id;
    hints
        .observe_retained(&db, &collision, endpoint(1))
        .await
        .unwrap();
    hints
        .inherit_retained(
            &db,
            &collision,
            HolderHint {
                endpoint: endpoint(1),
                expires: Instant::now() + LIFETIME,
            },
        )
        .await
        .unwrap();
    assert!(hints.entries.lock().unwrap().is_empty());
    hints
        .observe_retained(&db, &original, endpoint(1))
        .await
        .unwrap();
    assert!(
        hints
            .take(original.author(), original.event_id.to_short())
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_backoff_caps_at_sixty_seconds_and_attempt_expires() {
    let event = event(8, None);
    let hints = HolderHints::new();
    hints.observe(&event, endpoint(1));
    let mut expected_delay = Duration::from_secs(1);
    for _ in 0..9 {
        let hint = hints
            .take(event.author(), event.event_id.to_short())
            .unwrap();
        assert!(hint.timeout() <= OPERATION_TIMEOUT);
        assert!(
            hints
                .take(event.author(), event.event_id.to_short())
                .is_none()
        );
        tokio::time::advance(expected_delay).await;
        expected_delay = expected_delay
            .saturating_mul(2)
            .min(Duration::from_secs(60));
    }
    tokio::time::advance(LIFETIME).await;
    assert!(
        hints
            .take(event.author(), event.event_id.to_short())
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn slow_attempt_cancellation_starts_cooldown_at_release_and_prevents_overlap() {
    let event = event(10, None);
    let hints = HolderHints::new();
    hints.observe(&event, endpoint(1));
    let attempt = hints
        .attempt(event.author(), event.event_id.to_short())
        .unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    hints.observe(&event, endpoint(1));
    assert!(
        hints
            .attempt(event.author(), event.event_id.to_short())
            .is_none()
    );
    drop(attempt);
    assert!(
        hints
            .attempt(event.author(), event.event_id.to_short())
            .is_none()
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    hints
        .attempt(event.author(), event.event_id.to_short())
        .unwrap()
        .succeeded();
    assert!(
        hints
            .attempt(event.author(), event.event_id.to_short())
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_keeps_durable_missing_work_but_loses_only_runtime_routes() {
    let root = event(9, None);
    let child = event(9, Some(root.event_id.to_short()));
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = rostra_client_db::Database::open(&path, child.author())
        .await
        .unwrap();
    db.try_process_event(&child).await.unwrap();
    let hints = HolderHints::new();
    hints
        .observe_retained(&db, &child, endpoint(1))
        .await
        .unwrap();
    assert!(
        hints
            .take(child.author(), root.event_id.to_short())
            .is_some()
    );
    drop((hints, db));
    let db = rostra_client_db::Database::open(&path, child.author())
        .await
        .unwrap();
    let restarted = HolderHints::new();
    assert!(
        db.get_missing_events_for_id(child.author())
            .await
            .contains(&root.event_id.to_short())
    );
    assert!(
        restarted
            .take(child.author(), root.event_id.to_short())
            .is_none()
    );
    restarted
        .observe_retained(&db, &child, endpoint(1))
        .await
        .unwrap();
    assert!(
        restarted
            .take(child.author(), root.event_id.to_short())
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn duplicate_root_cannot_renew_an_evicted_descendants_deadline() {
    let root = event(11, None);
    let child = event(11, Some(root.event_id.to_short()));
    let hints = HolderHints::new();
    hints.observe(&child, endpoint(1));
    let deadline = hints.sources((child.author(), child.event_id.to_short()))[0].expires;
    hints
        .entries
        .lock()
        .unwrap()
        .remove(&(child.author(), root.event_id.to_short()));
    tokio::time::advance(Duration::from_secs(50)).await;
    hints.observe(&child, endpoint(1));
    assert_eq!(
        hints.sources((child.author(), root.event_id.to_short()))[0].expires,
        deadline
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frozen_scan_finishes_unchanged_batches_and_stops_after_known_edges() {
    let hints = HolderHints::new();
    let db = rostra_client_db::Database::new_in_memory(event(12, None).author())
        .await
        .unwrap();
    let mut entries = Vec::new();
    for marker in 20..52 {
        let missing = event(marker, None);
        hints.observe(&missing, endpoint(1));
        let key = (missing.author(), missing.event_id.to_short());
        entries.push((key, hints.sources(key)));
    }
    let grandparent = event(12, None);
    let parent = event(12, Some(grandparent.event_id.to_short()));
    let head = event(12, Some(parent.event_id.to_short()));
    db.try_process_event(&parent).await.unwrap();
    hints.observe(&head, endpoint(1));
    let key = (parent.author(), parent.event_id.to_short());
    entries.push((key, hints.sources(key)));
    let mut scan = RetainedHintScan::new(entries);
    hints.changed.notified().await;
    assert!(
        scan.step(&hints, &db).await.unwrap(),
        "first 32 unchanged entries are not exhaustion"
    );
    assert!(
        hints
            .sources((parent.author(), grandparent.event_id.to_short()))
            .is_empty()
    );
    assert!(!scan.step(&hints, &db).await.unwrap());
    assert_eq!(
        hints
            .sources((parent.author(), grandparent.event_id.to_short()))
            .len(),
        1
    );
    hints.changed.notified().await;
    let mut next = hints.retained_scan();
    while next.step(&hints, &db).await.unwrap() {}
    assert!(
        tokio::time::timeout(Duration::from_millis(10), hints.changed.notified())
            .await
            .is_err()
    );
    for _ in 0..100 {
        hints.observe(&head, endpoint(1));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(10), hints.changed.notified())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frozen_scan_rechecks_eviction_expiry_and_retained_signature() {
    let parent = event(13, Some(event(13, None).event_id.to_short()));
    let head = event(13, Some(parent.event_id.to_short()));
    for mode in 0..3 {
        let db = rostra_client_db::Database::new_in_memory(parent.author())
            .await
            .unwrap();
        let mut stored = parent;
        if mode == 2 {
            stored.sig = rostra_core::event::EventSignature::from_bytes([0; 64]);
        }
        db.try_process_event(&stored).await.unwrap();
        let hints = HolderHints::new();
        hints.observe(&head, endpoint(1));
        let mut scan = hints.retained_scan();
        if mode == 0 {
            hints.entries.lock().unwrap().clear();
        } else if mode == 1 {
            for holders in hints.entries.lock().unwrap().values_mut() {
                for holder in holders {
                    holder.hint.expires = Instant::now() - Duration::from_secs(1);
                }
            }
        }
        while scan.step(&hints, &db).await.unwrap() {}
        assert!(
            hints
                .sources((parent.author(), parent.parent_prev().unwrap()))
                .is_empty()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn long_retained_chain_finishes_without_duplicate_wake_continuation() {
    let secret = RostraIdSecretKey::from_bytes([14; 32]);
    let db = rostra_client_db::Database::new_in_memory(secret.id())
        .await
        .unwrap();
    let hints = HolderHints::new();
    let mut chain: Vec<VerifiedEvent> = Vec::new();
    for marker in 0..40 {
        let parent = chain.last().map(|event| event.event_id.to_short());
        let event = VerifiedEvent::verify_received_as_is(
            Event::builder_raw_content()
                .author(secret.id())
                .kind(EventKind::NULL)
                .content(&EventContentRaw::new(vec![marker]))
                .maybe_parent_prev(parent)
                .build()
                .signed_by(secret),
        )
        .unwrap();
        if !chain.is_empty() {
            db.try_process_event(&event).await.unwrap();
        }
        chain.push(event);
    }
    hints.observe(chain.last().unwrap(), endpoint(1));
    let original =
        hints.sources((secret.id(), chain.last().unwrap().event_id.to_short()))[0].expires;
    let mut passes = 0;
    while tokio::time::timeout(Duration::from_millis(10), hints.changed.notified())
        .await
        .is_ok()
    {
        passes += 1;
        assert!(
            passes <= chain.len() + 1,
            "finite verified chain must quiesce"
        );
        let mut scan = hints.retained_scan();
        while scan.step(&hints, &db).await.unwrap() {}
    }
    assert!(passes > 32, "exercise continuation beyond one small batch");
    let root = hints.sources((secret.id(), chain[0].event_id.to_short()));
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].expires, original);
    assert!(db.get_event(chain[0].event_id).await.is_none());
    hints.observe(chain.last().unwrap(), endpoint(1));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), hints.changed.notified())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_retained_cycle_is_an_error_not_a_recursive_routing_root() {
    let original = event(15, None);
    let mut cyclic = event(15, Some(original.event_id.to_short()));
    cyclic.event_id = original.event_id;
    let head = event(15, Some(original.event_id.to_short()));
    let db = rostra_client_db::Database::new_in_memory(original.author())
        .await
        .unwrap();
    db.try_process_event(&cyclic).await.unwrap();
    let hints = HolderHints::new();
    hints.observe(&head, endpoint(1));
    let mut scan = hints.retained_scan();
    assert!(scan.step(&hints, &db).await.is_err());
    assert_eq!(hints.entries.lock().unwrap().len(), 2);
}
