use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use rostra_client_db::{Database, EventContentState};
use rostra_core::ShortEventId;
use rostra_core::event::content_kind::{EventContentKind as _, Follow, PersonaSelector};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, IrohNodeId, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::connection::{
    Connection, FeedEventRequest, FeedEventResponse, MAX_REQUEST_SIZE, PingRequest, PingResponse,
    RpcId, RpcMessage as _,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument as _;
use tracing::instrument::WithSubscriber as _;

use super::{
    BROADCAST_POLICY, BroadcastHeadOutcome, BroadcastHeadReport, BroadcastPolicy,
    HeadUpdateBroadcaster, broadcast_retry_delay, content_completes_pending, content_is_terminal,
    log_broadcast_pass, reconcile_current_heads, take_one_ready_head,
};
use crate::Client;
use crate::task::diagnostic_test::{EventCapture, assert_no_forbidden_fields};

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

fn follow_event(
    id_secret: RostraIdSecretKey,
    followee: rostra_core::id::RostraId,
) -> VerifiedEventContent {
    let content = Follow {
        followee,
        persona: None,
        selector: Some(PersonaSelector::Except { ids: vec![] }),
        persona_tags_selector: None,
    }
    .serialize_cbor()
    .expect("follow content");
    let signed = Event::builder_raw_content()
        .author(id_secret.id())
        .kind(EventKind::FOLLOW)
        .content(&content)
        .build()
        .signed_by(id_secret);
    let event =
        VerifiedEvent::verify_signed(id_secret.id(), signed).expect("self-signed follow event");
    VerifiedEventContent::verify(event, content).expect("matching follow content")
}

#[tokio::test(flavor = "multi_thread")]
async fn ready_head_remains_pending_until_broadcast_succeeds() {
    let id_secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(id_secret.id())
        .await
        .expect("in-memory database");
    let (event, event_content) = build_event(id_secret, 1, None);
    let head = event.event_id.to_short();
    db.process_event(&event).await;

    let mut pending = BTreeSet::from([head]);
    assert!(
        take_one_ready_head(&db, &mut pending, &BTreeMap::new())
            .await
            .is_none()
    );
    assert_eq!(pending, BTreeSet::from([head]));

    db.process_event_content(&event_content).await;
    let ready = take_one_ready_head(&db, &mut pending, &BTreeMap::new())
        .await
        .expect("content-ready head");
    assert_eq!(ready.0, head);
    assert_eq!(pending, BTreeSet::from([head]));
    pending.remove(&head);
    assert!(pending.is_empty());
}

async fn retrying_feed_server(
    endpoint: iroh::Endpoint,
    attempts_tx: mpsc::UnboundedSender<usize>,
    resume: oneshot::Receiver<()>,
    completed_tx: oneshot::Sender<()>,
) {
    let incoming = endpoint.accept().await.expect("incoming connection");
    let connection = incoming
        .accept()
        .expect("accept connection")
        .await
        .expect("complete handshake");

    let (mut send, mut recv) = connection.accept_bi().await.expect("ping stream");
    let (rpc_id, request) = Connection::read_request_raw(&mut recv)
        .await
        .expect("ping request");
    assert_eq!(rpc_id, RpcId::PING);
    let request = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).expect("decode ping");
    Connection::write_success_return_code(&mut send)
        .await
        .expect("ping success");
    Connection::write_message(&mut send, &PingResponse(request.0))
        .await
        .expect("ping response");
    send.finish().expect("finish ping response");

    let mut resume = Some(resume);
    let mut completed_tx = Some(completed_tx);
    for attempt in 1..=3 {
        let (mut send, mut recv) = connection.accept_bi().await.expect("feed stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("feed request");
        assert_eq!(rpc_id, RpcId::FEED_EVENT);
        attempts_tx.send(attempt).expect("attempt receiver");
        if let Some(resume) = resume.take() {
            resume.await.expect("resume retrying server");
            continue;
        }
        if attempt == 3 {
            std::future::pending::<()>().await;
        }
        let FeedEventRequest(event) =
            FeedEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).expect("decode feed");
        Connection::write_success_return_code(&mut send)
            .await
            .expect("feed success");
        Connection::write_message(&mut send, &FeedEventResponse)
            .await
            .expect("feed response");
        Connection::read_bao_content(&mut recv, event.content_len(), event.content_hash())
            .await
            .expect("feed trailer");
        Connection::write_success_return_code(&mut send)
            .await
            .expect("feed trailer success");
        completed_tx
            .take()
            .expect("completion signal")
            .send(())
            .expect("successful feed completion receiver");
    }
    std::future::pending::<()>().await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn hanging_follower_does_not_block_later_follower_and_head_retries() {
    let hang_secret = RostraIdSecretKey::generate();
    let hang_id = hang_secret.id();
    let responsive_secret = RostraIdSecretKey::generate();
    let responsive_id = responsive_secret.id();
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();

    let hanging_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("hanging endpoint");
    let hanging_node_id = hanging_endpoint.id();
    lookup.add_endpoint_info(hanging_endpoint.addr());
    let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let (completed_tx, completed_rx) = oneshot::channel();
    let hanging_server = tokio::spawn(retrying_feed_server(
        hanging_endpoint,
        attempts_tx,
        resume_rx,
        completed_tx,
    ));

    let responsive_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("responsive endpoint");
    let responsive_node_id = responsive_endpoint.id();
    lookup.add_endpoint_info(responsive_endpoint.addr());
    let responsive = Client::builder(responsive_id)
        .db(Database::new_in_memory(responsive_id)
            .await
            .expect("responsive database"))
        .iroh_endpoint(responsive_endpoint)
        .start_background_tasks(false)
        .build()
        .await
        .expect("responsive client");
    responsive
        .db()
        .process_event_with_content(&follow_event(responsive_secret, hang_id))
        .await;

    let broadcaster_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("broadcaster endpoint");
    let broadcaster = Client::builder(hang_id)
        .db(Database::new_in_memory(hang_id)
            .await
            .expect("broadcaster database"))
        .iroh_endpoint(broadcaster_endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("broadcaster client");
    let db = broadcaster.db();
    db.insert_id_node(
        hang_id,
        IrohNodeId::from_bytes(*hanging_node_id.as_bytes()),
        rostra_core::Timestamp::now(),
    )
    .await;
    db.insert_id_node(
        responsive_id,
        IrohNodeId::from_bytes(*responsive_node_id.as_bytes()),
        rostra_core::Timestamp::now(),
    )
    .await;
    db.process_event_with_content(&follow_event(responsive_secret, hang_id))
        .await;
    let (event, event_content) = build_event(hang_secret, 1, None);
    let head = event.event_id.to_short();
    db.process_event_with_content(&event_content).await;
    let policy = BroadcastPolicy {
        peer_deadline: Duration::from_secs(1),
        retry_initial_delay: Duration::from_millis(100),
        retry_max_delay: Duration::from_millis(100),
    };
    let capture = EventCapture::default();
    let broadcaster_task = HeadUpdateBroadcaster::new(&broadcaster);
    let worker = tokio::spawn(
        async move {
            let identity_span = tracing::info_span!("test-identity", self_id = %hang_id.to_short());
            broadcaster_task
                .run_with_policy(policy)
                .instrument(identity_span)
                .await;
        }
        .with_subscriber(capture.subscriber()),
    );

    tokio::time::timeout(Duration::from_secs(3), attempts_rx.recv())
        .await
        .expect("first feed request")
        .expect("first feed attempt");
    tokio::time::timeout(Duration::from_secs(3), async {
        while !responsive.db().has_event(head).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("later responsive follower receives the head");
    resume_tx.send(()).expect("resume retrying server");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), attempts_rx.recv())
            .await
            .is_err(),
        "retry waits for the configured backoff"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), attempts_rx.recv())
            .await
            .expect("retry feed request"),
        Some(2),
        "the pending head is retried after the first deadline"
    );
    tokio::time::timeout(Duration::from_secs(1), completed_rx)
        .await
        .expect("the retry completes its typed FEED exchange")
        .expect("successful feed completion");
    assert!(
        !matches!(
            tokio::time::timeout(Duration::from_millis(300), attempts_rx.recv()).await,
            Ok(Some(_))
        ),
        "successful retry removes the pending head"
    );
    let passes = capture.events("Head broadcast pass completed");
    assert_eq!(passes.len(), 2);
    assert_eq!(passes[0].target, "rostra::head_broadcaster");
    assert_eq!(passes[0].fields.get("head"), Some(&head.to_string()));
    assert_eq!(
        passes[0].fields.get("recipients").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        passes[0].fields.get("delivered").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        passes[0].fields.get("errors").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        passes[0].fields.get("timeouts").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        passes[0].fields.get("attempt").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        passes[0].fields.get("outcome").map(String::as_str),
        Some("retry")
    );
    assert_eq!(
        passes[0].fields.get("retry_delay_ms").map(String::as_str),
        Some("100")
    );
    assert!(passes[0].fields.contains_key("elapsed_ms"));
    assert_eq!(
        passes[1].fields.get("attempt").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        passes[1].fields.get("outcome").map(String::as_str),
        Some("delivered")
    );
    assert_eq!(
        passes[1].fields.get("retry_delay_ms").map(String::as_str),
        Some("0")
    );
    for pass in &passes {
        assert_no_forbidden_fields(pass);
    }
    let identity_spans = capture.spans("test-identity");
    assert_eq!(identity_spans.len(), 1);
    assert!(identity_spans[0].fields.contains_key("self_id"));

    worker.abort();
    assert!(
        worker
            .await
            .expect_err("worker is cancelled")
            .is_cancelled()
    );
    hanging_server.abort();
    assert!(
        hanging_server
            .await
            .expect_err("server is cancelled")
            .is_cancelled()
    );
    drop(broadcaster);
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

    let first = take_one_ready_head(&db, &mut pending, &BTreeMap::new())
        .await
        .expect("one ready head");
    assert!(expected.contains(&first.0));
    assert_eq!(pending.len(), expected.len());

    let mut ready = BTreeSet::from([first.0]);
    pending.remove(&first.0);
    while let Some((head, _, _)) = take_one_ready_head(&db, &mut pending, &BTreeMap::new()).await {
        ready.insert(head);
        pending.remove(&head);
    }
    assert_eq!(ready, expected);
    assert!(pending.is_empty());

    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let receiver_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("self endpoint");
    let receiver_node_id = receiver_endpoint.id();
    lookup.add_endpoint_info(receiver_endpoint.addr());
    let receiver = Client::builder(id_secret.id())
        .db(Database::new_in_memory(id_secret.id())
            .await
            .expect("receiver database"))
        .iroh_endpoint(receiver_endpoint)
        .start_background_tasks(false)
        .build()
        .await
        .expect("receiver client");
    let broadcaster_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("broadcaster endpoint");
    let client = Client::builder(id_secret.id())
        .db(db)
        .iroh_endpoint(broadcaster_endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("self client");
    client
        .db()
        .insert_id_node(
            id_secret.id(),
            IrohNodeId::from_bytes(*receiver_node_id.as_bytes()),
            rostra_core::Timestamp::now(),
        )
        .await;
    let capture = EventCapture::default();
    let broadcaster_task = HeadUpdateBroadcaster::new(&client);
    let worker = tokio::spawn(
        async move {
            let identity_span =
                tracing::info_span!("test-identity", self_id = %id_secret.id().to_short());
            broadcaster_task
                .run_with_policy(BROADCAST_POLICY)
                .instrument(identity_span)
                .await;
        }
        .with_subscriber(capture.subscriber()),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while capture.events("Head broadcast pass completed").len() < expected.len() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all reconciled sibling passes");
    let passes = capture.events("Head broadcast pass completed");
    assert_eq!(passes.len(), expected.len());
    assert_eq!(
        passes
            .iter()
            .filter_map(|pass| pass.fields.get("head").cloned())
            .collect::<BTreeSet<_>>(),
        expected.iter().map(ToString::to_string).collect()
    );
    assert!(passes.iter().all(|pass| {
        pass.fields.get("outcome").map(String::as_str) == Some("delivered")
            && pass.fields.get("recipients").map(String::as_str) == Some("1")
            && pass.fields.get("attempt").map(String::as_str) == Some("1")
    }));
    for pass in &passes {
        assert_no_forbidden_fields(pass);
    }
    let identity_spans = capture.spans("test-identity");
    assert_eq!(identity_spans.len(), 1);
    assert!(identity_spans[0].fields.contains_key("self_id"));
    for head in expected {
        assert!(receiver.db().has_event(head).await);
    }
    worker.abort();
    assert!(
        worker
            .await
            .expect_err("worker is cancelled")
            .is_cancelled()
    );
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
    assert!(
        take_one_ready_head(&db, &mut pending, &BTreeMap::new())
            .await
            .is_none()
    );
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

#[test]
fn broadcast_retry_delay_is_capped() {
    assert_eq!(
        broadcast_retry_delay(0, BROADCAST_POLICY),
        Duration::from_secs(1)
    );
    assert_eq!(
        broadcast_retry_delay(1, BROADCAST_POLICY),
        Duration::from_secs(2)
    );
    assert_eq!(
        broadcast_retry_delay(6, BROADCAST_POLICY),
        Duration::from_secs(60)
    );
    assert_eq!(
        broadcast_retry_delay(u32::MAX, BROADCAST_POLICY),
        Duration::from_secs(60)
    );

    let capture = EventCapture::default();
    tracing::subscriber::with_default(capture.subscriber(), || {
        let span = tracing::info_span!("test-identity", self_id = "forbidden");
        span.in_scope(|| {
            log_broadcast_pass(
                ShortEventId::ZERO,
                3,
                u32::MAX,
                BroadcastHeadReport {
                    outcome: BroadcastHeadOutcome::Retry,
                    delivered: 1,
                    errors: 1,
                    timeouts: 1,
                },
                Duration::from_secs(2),
                Some(broadcast_retry_delay(u32::MAX, BROADCAST_POLICY)),
            );
        });
    });
    let passes = capture.events("Head broadcast pass completed");
    assert_eq!(passes.len(), 1);
    assert_eq!(
        passes[0].fields.get("retry_delay_ms").map(String::as_str),
        Some("60000")
    );
    assert_eq!(
        passes[0].fields.get("outcome").map(String::as_str),
        Some("retry")
    );
    assert_no_forbidden_fields(&passes[0]);
}
