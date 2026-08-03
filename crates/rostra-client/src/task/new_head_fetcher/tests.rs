use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent};
use rostra_core::id::RostraIdSecretKey;
use tempfile::tempdir;
use tokio::sync::broadcast;

use super::*;

#[test]
fn dropping_work_lease_releases_author_and_preserves_pending_retry() {
    let author = RostraIdSecretKey::from_bytes([31; 32]).id();
    let (queue, _notifications) = WorkQueue::new();
    assert!(queue.try_enqueue(author));

    let lease = queue.take_work().expect("queued work");
    assert!(queue.try_enqueue(author));
    assert!(queue.take_work().is_none());

    drop(lease);
    let retried = queue.take_work().expect("released work can be retried");
    assert_eq!(retried.author, author);
}

#[test]
fn pending_author_queue_is_bounded() {
    let (queue, _notifications) = WorkQueue::new();
    for byte in 0..MAX_PENDING_AUTHORS {
        let author = RostraIdSecretKey::from_bytes([byte as u8; 32]).id();
        assert!(queue.try_enqueue(author));
    }
    let overflow = RostraIdSecretKey::from_bytes([MAX_PENDING_AUTHORS as u8; 32]).id();
    assert!(!queue.try_enqueue(overflow));

    drop(queue.take_work().expect("queued work"));
    assert!(queue.try_enqueue(overflow));
}

#[test]
fn full_irrelevant_page_continues_to_later_durable_head() {
    let self_id = RostraIdSecretKey::from_bytes([201; 32]).id();
    let irrelevant: Vec<_> = (0..MAX_PENDING_AUTHORS)
        .map(|index| RostraIdSecretKey::from_bytes([index as u8; 32]).id())
        .collect();
    let (queue, _notifications) = WorkQueue::new();
    let mut cursor = None;

    assert_eq!(
        enqueue_reconciled_authors(
            &queue,
            irrelevant,
            false,
            &WotData::default(),
            self_id,
            &mut cursor,
        ),
        HeadReconcileOutcome::MoreCandidates
    );
    assert!(queue.take_work().is_none());
    assert_eq!(
        enqueue_reconciled_authors(
            &queue,
            vec![self_id],
            true,
            &WotData::default(),
            self_id,
            &mut cursor,
        ),
        HeadReconcileOutcome::Complete
    );
    assert_eq!(
        queue.take_work().expect("later durable head").author,
        self_id
    );
}

#[test]
fn admission_racing_with_full_scan_remains_an_incremental_entrant() {
    let self_id = RostraIdSecretKey::from_bytes([202; 32]).id();
    let admitted = RostraIdSecretKey::from_bytes([203; 32]).id();
    let baseline = WotData::default();
    let mut updated = WotData::default();
    updated.extended.insert(admitted);
    let (queue, _notifications) = WorkQueue::new();
    let mut cursor = None;

    assert_eq!(
        enqueue_reconciled_authors(
            &queue,
            vec![admitted],
            true,
            &baseline,
            self_id,
            &mut cursor,
        ),
        HeadReconcileOutcome::Complete
    );
    assert!(queue.take_work().is_none());
    assert_eq!(
        newly_admitted_authors(&baseline, &updated, self_id).collect::<Vec<_>>(),
        vec![admitted]
    );
    assert!(queue.try_enqueue(admitted));
    assert_eq!(queue.take_work().expect("admitted author").author, admitted);
}

#[test]
fn coalesced_remove_and_readmission_requires_durable_rescan() {
    let self_id = RostraIdSecretKey::from_bytes([204; 32]).id();
    let author = RostraIdSecretKey::from_bytes([205; 32]).id();
    let mut baseline = WotData::default();
    baseline.extended.insert(author);
    let removed = WotData::default();
    let final_snapshot = baseline.clone();
    let (queue, _notifications) = WorkQueue::new();
    let mut cursor = None;

    assert_eq!(
        enqueue_reconciled_authors(&queue, vec![author], true, &removed, self_id, &mut cursor,),
        HeadReconcileOutcome::Complete
    );
    assert!(queue.take_work().is_none());
    assert!(
        newly_admitted_authors(&baseline, &final_snapshot, self_id)
            .next()
            .is_none()
    );

    assert_eq!(
        enqueue_reconciled_authors(
            &queue,
            vec![author],
            true,
            &final_snapshot,
            self_id,
            &mut cursor,
        ),
        HeadReconcileOutcome::Complete
    );
    assert_eq!(
        queue
            .take_work()
            .expect("coalesced membership rescan")
            .author,
        author
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lag_reconciliation_enqueues_current_durable_head() {
    let secret = RostraIdSecretKey::from_bytes([32; 32]);
    let author = secret.id();
    let dir = tempdir().expect("temporary directory");
    let db = Database::open(dir.path().join("db.redb"), author)
        .await
        .expect("database");
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&EventContentRaw::new(vec![]))
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(author, event).expect("valid event");
    db.process_event(&event).await;

    let (sender, mut receiver) = broadcast::channel(1);
    sender.send((author, ShortEventId::ZERO)).expect("receiver");
    sender.send((author, ShortEventId::MAX)).expect("receiver");
    assert!(matches!(
        receiver.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));

    let (queue, _notifications) = WorkQueue::new();
    let mut cursor = None;
    let authors = db.get_ids_with_heads(None, MAX_PENDING_AUTHORS).await;
    assert_eq!(
        enqueue_reconciled_authors(
            &queue,
            authors,
            true,
            &db.self_wot_subscribe().snapshot(),
            author,
            &mut cursor,
        ),
        HeadReconcileOutcome::Complete
    );

    assert_eq!(queue.take_work().expect("durable head work").author, author);
}
