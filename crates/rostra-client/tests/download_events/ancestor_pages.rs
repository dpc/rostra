use rostra_core::event::SignedEvent;
use rostra_p2p::connection::{
    ANCESTOR_PAGE_BYTES, ANCESTOR_PAGE_EVENTS, GetAncestorsRequest, GetAncestorsResponse,
};

use super::*;

fn chain(secret: RostraIdSecretKey, count: usize) -> Vec<VerifiedEventContent> {
    let mut result = Vec::new();
    let mut parent = None;
    for n in 0..count {
        let (event, content) =
            build_test_event_with_text(secret, parent, &format!("ancestor page {n}"));
        parent = Some(event.event_id.to_short());
        result.push(content);
    }
    result
}

fn page(events: impl IntoIterator<Item = VerifiedEvent>) -> GetAncestorsResponse {
    let mut page = GetAncestorsResponse([None; ANCESTOR_PAGE_EVENTS]);
    for (slot, event) in page.0.iter_mut().zip(events) {
        *slot = Some(event.into());
    }
    page
}

async fn production_peer(
    author: RostraId,
    contents: &[VerifiedEventContent],
) -> (Arc<Client>, Connection, iroh::Endpoint) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    lookup.add_endpoint_info(server.addr());
    let id = server.id();
    let server = Client::builder(author)
        .db(Database::new_in_memory(author).await.unwrap())
        .iroh_endpoint(server)
        .start_background_tasks(false)
        .build()
        .await
        .unwrap();
    for content in contents {
        server.db().try_process_event(&content.event).await.unwrap();
    }
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .address_lookup(lookup)
        .bind()
        .await
        .unwrap();
    let conn = Connection::from(endpoint.connect(id, ROSTRA_P2P_V0_ALPN).await.unwrap());
    (server, conn, endpoint)
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn production_pages_bound_long_chains_and_preserve_missing_roots() {
    let secret = RostraIdSecretKey::generate();
    let events = chain(secret, 130);
    let (_server, conn, _endpoint) = production_peer(secret.id(), &events).await;
    let mut root = events.last().unwrap().event_id().to_short();
    let mut received = Vec::new();
    let mut rounds = 0;
    loop {
        let headers = conn.get_ancestors(secret.id(), root).await.unwrap();
        assert!(!headers.is_empty());
        assert!(headers.len() <= ANCESTOR_PAGE_EVENTS);
        rounds += 1;
        let next = headers.last().unwrap().parent_prev();
        received.extend(headers.into_iter().map(|event| event.event_id.to_short()));
        let Some(next) = next else { break };
        root = next;
    }
    assert_eq!(rounds, 3);
    assert_eq!(
        received,
        events
            .iter()
            .rev()
            .map(|event| event.event_id().to_short())
            .collect::<Vec<_>>()
    );
    let absent = build_test_event(secret, None).0.event_id.to_short();
    assert!(
        conn.get_ancestors(secret.id(), absent)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        conn.get_ancestors(RostraIdSecretKey::generate().id(), root)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn production_branching_shared_and_absent_parents_consume_bounded_work() {
    let secret = RostraIdSecretKey::generate();
    let (base, base_content) = build_test_event(secret, None);
    let (left, left_content) = build_test_event_with_text(secret, base.event_id.to_short(), "left");
    let (right, right_content) =
        build_test_event_with_text(secret, base.event_id.to_short(), "right");
    let (head, head_content) = build_test_event_with_parents(
        secret,
        left.event_id.to_short(),
        right.event_id.to_short(),
        "merge",
    );
    let contents = [base_content, left_content, right_content, head_content];
    let (server, conn, _endpoint) = production_peer(secret.id(), &contents).await;
    let headers = conn
        .get_ancestors(secret.id(), head.event_id.to_short())
        .await
        .unwrap();
    assert_eq!(headers.len(), 4);
    assert_eq!(
        headers
            .iter()
            .map(|event| event.event_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );

    // Every spine node contributes an absent auxiliary parent. Work counts
    // misses too, rather than searching without bound to fill the page.
    let mut parent = None;
    for n in 0..70 {
        let missing = build_test_event_with_text(secret, None, &format!("absent {n}")).0;
        let (event, _) = build_test_event_with_parents(
            secret,
            parent,
            missing.event_id.to_short(),
            &format!("spine {n}"),
        );
        server.db().try_process_event(&event).await.unwrap();
        parent = Some(event.event_id.to_short());
    }
    let headers = conn
        .get_ancestors(secret.id(), parent.unwrap())
        .await
        .unwrap();
    assert_eq!(headers.len(), ANCESTOR_PAGE_EVENTS / 2);
    // Cross-author rows do not satisfy a parent, nor expose their own ancestors.
    let foreign = build_test_event(RostraIdSecretKey::generate(), None).0;
    server.db().try_process_event(&foreign).await.unwrap();
    let (head, _) = build_test_event(secret, foreign.event_id.to_short());
    server.db().try_process_event(&head).await.unwrap();
    assert_eq!(
        conn.get_ancestors(secret.id(), head.event_id.to_short())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn historical_discovery_uses_three_pages_instead_of_130_header_rounds() {
    let secret = RostraIdSecretKey::generate();
    let events = chain(secret, 130);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let pages = events
        .iter()
        .enumerate()
        .map(|(n, event)| {
            (
                event.event_id().to_short(),
                page(events[..=n].iter().rev().map(|event| event.event)),
            )
        })
        .collect();
    let peer =
        RecordedPeer::start_config(lookup.clone(), events.clone(), HashMap::new(), Some(pages))
            .await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    assert!(
        client
            .sync_event_from_peers(
                secret.id(),
                events.last().unwrap().event_id().to_short(),
                &[secret.id()],
            )
            .await
            .unwrap()
    );
    assert_eq!(peer.requests.ancestors.lock().unwrap().len(), 3);
    assert!(peer.requests.events().is_empty());
    assert_eq!(peer.requests.contents().len(), events.len());
    for event in events {
        assert!(
            client
                .db()
                .get_event_content(event.event_id().to_short())
                .await
                .is_some()
        );
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn old_peer_is_probed_once_across_traversals() {
    let secret = RostraIdSecretKey::generate();
    let events = chain(secret, 9);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(lookup.clone(), events.clone()).await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    // Two independent cold roots on the same cached connection.
    for root in [events[3].event_id(), events[8].event_id()] {
        client
            .sync_event_from_peers(secret.id(), root.to_short(), &[secret.id()])
            .await
            .unwrap();
    }
    assert_eq!(peer.requests.ancestors.lock().unwrap().len(), 1);
    assert_eq!(peer.requests.events().len(), events.len());
}

#[test]
fn entire_page_requires_valid_signatures_author_identity_and_root_connectivity() {
    let secret = RostraIdSecretKey::generate();
    let events = chain(secret, 3);
    let root = events[2].event_id().to_short();
    assert_eq!(
        page(events.iter().rev().map(|event| event.event))
            .verify(secret.id(), root)
            .unwrap()
            .len(),
        3
    );
    let unrelated = build_test_event_with_text(secret, None, "unrelated").0;
    assert!(
        page([events[2].event, unrelated])
            .verify(secret.id(), root)
            .is_err()
    );
    assert!(page([events[1].event]).verify(secret.id(), root).is_err());
    assert!(
        page([events[2].event, events[2].event])
            .verify(secret.id(), root)
            .is_err()
    );
    assert!(
        page([events[2].event])
            .verify(RostraIdSecretKey::generate().id(), root)
            .is_err()
    );
    let mut invalid = page([events[2].event, events[1].event]);
    invalid.0[1].as_mut().unwrap().sig = SignedEvent::from(unrelated).sig;
    assert!(invalid.verify(secret.id(), root).is_err());
    let mut gap = page([events[2].event]);
    gap.0[2] = Some(events[1].event.into());
    assert!(gap.verify(secret.id(), root).is_err());
    // All wire header fields are fixed-width; even the maximal page fits the
    // explicit frame bound. No collection length from the wire allocates memory.
    let full = GetAncestorsResponse([Some(events[0].event.into()); ANCESTOR_PAGE_EVENTS]);
    let encoded = bincode::encode_to_vec(full, rostra_core::bincode::STD_BINCODE_CONFIG).unwrap();
    assert!(encoded.len() < ANCESTOR_PAGE_BYTES as usize);
    assert!(
        GetAncestorsResponse::decode_whole::<ANCESTOR_PAGE_BYTES>(&vec![
            0xff;
            ANCESTOR_PAGE_BYTES
                as usize
        ])
        .is_err()
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn invalid_page_is_discarded_before_ingestion_and_legacy_fallback_is_verified() {
    let secret = RostraIdSecretKey::generate();
    let (root, content) = build_test_event(secret, None);
    let unrelated = build_test_event_with_text(secret, None, "unrelated").0;
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start_config(
        lookup.clone(),
        [content],
        HashMap::new(),
        Some(HashMap::from([(
            root.event_id.to_short(),
            page([root, unrelated]),
        )])),
    )
    .await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    assert!(
        client
            .sync_event_from_peers(secret.id(), root.event_id.to_short(), &[secret.id()])
            .await
            .unwrap()
    );
    assert_eq!(peer.requests.events(), [root.event_id.to_short()]);
    assert!(!client.db().has_event(unrelated.event_id.to_short()).await);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn partial_pages_preserve_durable_missing_parents_and_use_local_headers() {
    let secret = RostraIdSecretKey::generate();
    let events = chain(secret, 4);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let root = events[3].event_id().to_short();
    let peer = RecordedPeer::start_config(
        lookup.clone(),
        events[1..].iter().cloned(),
        HashMap::new(),
        Some(HashMap::from([(
            root,
            page(events[1..].iter().rev().map(|event| event.event)),
        )])),
    )
    .await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    // A locally retained envelope must consume its cached copy rather than
    // leaving lookahead permanently occupied or fetching that page again.
    client
        .db()
        .try_process_event(&events[2].event)
        .await
        .unwrap();
    assert!(
        client
            .sync_event_from_peers(secret.id(), root, &[secret.id()])
            .await
            .unwrap()
    );
    assert_eq!(
        *peer.requests.ancestors.lock().unwrap(),
        [root, events[0].event_id().to_short()]
    );
    assert!(peer.requests.events().is_empty());
    assert_eq!(
        client.db().get_missing_events_for_id(secret.id()).await,
        [events[0].event_id().to_short()]
    );
    for event in &events[1..] {
        assert!(
            client
                .db()
                .get_event_content(event.event_id().to_short())
                .await
                .is_some()
        );
    }
}

/// Serve a hostile length or stall until the receiver cancels its stream.
async fn wire_peer(
    oversized: bool,
    entered: Arc<Semaphore>,
    stopped: Arc<Semaphore>,
) -> (Connection, iroh::Endpoint, tokio::task::JoinHandle<()>) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .bind()
        .await
        .unwrap();
    lookup.add_endpoint_info(server.addr());
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .address_lookup(lookup)
        .bind()
        .await
        .unwrap();
    let server_id = server.id();
    let task = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        loop {
            let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                break;
            };
            let (id, request) = Connection::read_request_raw(&mut recv).await.unwrap();
            match id {
                RpcId::GET_ANCESTORS => {
                    GetAncestorsRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).unwrap();
                    entered.add_permits(1);
                    if oversized {
                        Connection::write_success_return_code(&mut send)
                            .await
                            .unwrap();
                        send.write_all(&(ANCESTOR_PAGE_BYTES + 1).to_be_bytes())
                            .await
                            .unwrap();
                    }
                    // STOP_SENDING proves that cancellation/oversize rejection
                    // released the receiving stream, not merely the Rust future.
                    send.stopped().await.unwrap();
                    stopped.add_permits(1);
                }
                RpcId::PING => {
                    let request = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).unwrap();
                    Connection::write_success_return_code(&mut send)
                        .await
                        .unwrap();
                    Connection::write_message(&mut send, &PingResponse(request.0))
                        .await
                        .unwrap();
                }
                _ => panic!("unexpected wire request"),
            }
        }
        server.close().await;
    });
    let conn = Connection::from(
        endpoint
            .connect(server_id, ROSTRA_P2P_V0_ALPN)
            .await
            .unwrap(),
    );
    (conn, endpoint, task)
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn concurrent_and_cancelled_page_probes_release_streams_and_do_not_repeat() {
    let entered = Arc::new(Semaphore::new(0));
    let stopped = Arc::new(Semaphore::new(0));
    let (conn, endpoint, server) = wire_peer(false, entered.clone(), stopped.clone()).await;
    let author = RostraIdSecretKey::generate();
    let root = build_test_event(author, None).0.event_id.to_short();
    let clone = conn.clone();
    let pending = tokio::spawn(async move { clone.get_ancestors(author.id(), root).await });
    tokio::time::timeout(Duration::from_secs(5), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    // Concurrent traversal immediately takes legacy fallback without probing.
    assert!(conn.get_ancestors(author.id(), root).await.is_none());
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), stopped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(conn.get_ancestors(author.id(), root).await.is_none());
    assert_eq!(entered.available_permits(), 0);
    assert_eq!(conn.ping(77).await.unwrap(), 77);
    endpoint.close().await;
    server.await.unwrap();
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn oversized_and_stalled_pages_disable_further_probes() {
    for oversized in [true, false] {
        let entered = Arc::new(Semaphore::new(0));
        let stopped = Arc::new(Semaphore::new(0));
        let (conn, endpoint, server) = wire_peer(oversized, entered.clone(), stopped.clone()).await;
        let author = RostraIdSecretKey::generate();
        let root = build_test_event(author, None).0.event_id.to_short();
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                conn.get_ancestors(author.id(), root)
            )
            .await
            .unwrap()
            .is_none()
        );
        tokio::time::timeout(Duration::from_secs(5), stopped.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        assert!(conn.get_ancestors(author.id(), root).await.is_none());
        assert_eq!(entered.available_permits(), 1);
        assert_eq!(conn.ping(88).await.unwrap(), 88);
        endpoint.close().await;
        server.await.unwrap();
    }
}
