use std::collections::BTreeSet;

use rostra_client_db::{Database, EventContentState};
use rostra_core::ShortEventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use super::{
    content_completes_pending, content_is_terminal, reconcile_current_heads, take_one_ready_head,
};

fn build_event(
    id_secret: RostraIdSecretKey,
    content_byte: u8,
    parent_prev: Option<ShortEventId>,
) -> (VerifiedEvent, VerifiedEventContent) {
    let content = EventContentRaw::new(vec![content_byte]);
    let signed = Event::builder_raw_content()
        .author(id_secret.id())
        .kind(EventKind::NULL)
        .maybe_parent_prev(parent_prev)
        .content(&content)
        .build()
        .signed_by(id_secret);
    let event =
        VerifiedEvent::verify_signed(id_secret.id(), signed).expect("self-signed test event");
    let event_content =
        VerifiedEventContent::verify(event, content).expect("matching test content");
    (event, event_content)
}

#[tokio::test(flavor = "multi_thread")]
async fn retains_header_first_head_until_content_is_ready() {
    let id_secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(id_secret.id())
        .await
        .expect("in-memory database");
    let (event, event_content) = build_event(id_secret, 1, None);
    let head = event.event_id.to_short();
    db.process_event(&event).await;

    let mut pending = BTreeSet::from([head]);
    assert!(take_one_ready_head(&db, &mut pending).await.is_none());
    assert_eq!(pending, BTreeSet::from([head]));

    db.process_event_content(&event_content).await;
    let ready = take_one_ready_head(&db, &mut pending)
        .await
        .expect("content-ready head");
    assert_eq!(ready.0, head);
    assert!(pending.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn complete_reconciliation_recovers_startup_siblings_and_deduplicates() {
    let id_secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(id_secret.id())
        .await
        .expect("in-memory database");
    let mut expected = BTreeSet::new();
    for content_byte in 0..3 {
        let (_, event_content) = build_event(id_secret, content_byte, None);
        expected.insert(event_content.event.event_id.to_short());
        db.process_event_with_content(&event_content).await;
    }

    let mut pending = BTreeSet::new();
    reconcile_current_heads(&db, &mut pending).await;
    reconcile_current_heads(&db, &mut pending).await;
    assert_eq!(pending, expected);

    let first = take_one_ready_head(&db, &mut pending)
        .await
        .expect("one ready head");
    assert!(expected.contains(&first.0));
    assert_eq!(pending.len(), expected.len() - 1);

    let mut ready = BTreeSet::from([first.0]);
    while let Some((head, _, _)) = take_one_ready_head(&db, &mut pending).await {
        ready.insert(head);
    }
    assert_eq!(ready, expected);
    assert!(pending.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn long_header_only_chain_retains_only_current_head() {
    let id_secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(id_secret.id())
        .await
        .expect("in-memory database");
    let mut parent = None;
    let mut pending = BTreeSet::new();

    for content_byte in 0..100 {
        let (event, _) = build_event(id_secret, content_byte, parent);
        parent = Some(event.event_id.to_short());
        db.process_event(&event).await;
        reconcile_current_heads(&db, &mut pending).await;
        assert_eq!(pending, BTreeSet::from([parent.expect("just assigned")]));
    }

    let unrelated_secret = RostraIdSecretKey::generate();
    let (_, unrelated_content) = build_event(unrelated_secret, 1, None);
    assert!(!content_completes_pending(
        &unrelated_content,
        id_secret.id(),
        &pending
    ));

    let (_, nonhead_content) = build_event(id_secret, 200, None);
    assert!(!content_completes_pending(
        &nonhead_content,
        id_secret.id(),
        &pending
    ));
    assert!(take_one_ready_head(&db, &mut pending).await.is_none());
    assert_eq!(pending.len(), 1);
}

#[test]
fn processed_content_state_is_not_misclassified_as_terminal() {
    assert!(!content_is_terminal(None));
    assert!(!content_is_terminal(Some(EventContentState::Missing {
        last_fetch_attempt: None,
        fetch_attempt_count: 0,
        next_fetch_attempt: rostra_core::Timestamp::ZERO,
    })));
    assert!(content_is_terminal(Some(EventContentState::Pruned)));
    assert!(content_is_terminal(Some(EventContentState::Invalid)));
    assert!(content_is_terminal(Some(EventContentState::Deleted {
        deleted_by: ShortEventId::ZERO,
    })));
}
