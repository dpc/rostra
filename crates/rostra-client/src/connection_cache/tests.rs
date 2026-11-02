use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use rostra_client_db::Database;
use rostra_core::event::{Event, EventContentRaw, EventKind, IrohNodeId, SignedEvent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::connection::{
    Connection, GetEventContentRequest, GetEventContentResponse, GetEventRequest, GetEventResponse,
    MAX_REQUEST_SIZE, PingRequest, PingResponse, RpcId, RpcMessage as _,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{Notify, OnceCell, oneshot};

use super::{CLEANUP_INTERVAL, ConnectionCache};
use crate::Client;

const TEST_PEER_DEADLINE: Duration = Duration::from_secs(1);

#[derive(Clone)]
enum PeerBehavior {
    HangPing,
    HangEvent,
    Missing,
    Event(SignedEvent),
    Content(EventContentRaw),
    GatedContent(EventContentRaw, Arc<Notify>),
    HangContent,
}

struct TestPeer {
    endpoint: iroh::Endpoint,
    behavior: PeerBehavior,
    event_request_received: Arc<Notify>,
    content_request_received: Arc<Notify>,
}

async fn handle_test_peer_rpc(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    behavior: PeerBehavior,
    event_request_received: Arc<Notify>,
    content_request_received: Arc<Notify>,
) {
    let (rpc_id, request) = Connection::read_request_raw(&mut recv)
        .await
        .expect("RPC request");
    match rpc_id {
        RpcId::PING => {
            let request =
                PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).expect("decode ping");
            if matches!(behavior, PeerBehavior::HangPing) {
                event_request_received.notify_one();
                std::future::pending::<()>().await;
            }
            Connection::write_success_return_code(&mut send)
                .await
                .expect("ping success");
            Connection::write_message(&mut send, &PingResponse(request.0))
                .await
                .expect("ping response");
            send.finish().expect("finish ping response");
        }
        RpcId::GET_EVENT => {
            GetEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).expect("decode GET_EVENT");
            event_request_received.notify_one();
            match behavior {
                PeerBehavior::HangEvent => std::future::pending::<()>().await,
                PeerBehavior::Missing
                | PeerBehavior::HangPing
                | PeerBehavior::Content(_)
                | PeerBehavior::GatedContent(_, _)
                | PeerBehavior::HangContent => {
                    Connection::write_success_return_code(&mut send)
                        .await
                        .expect("GET_EVENT success");
                    Connection::write_message(&mut send, &GetEventResponse(None))
                        .await
                        .expect("missing response");
                    send.finish().expect("finish missing response");
                }
                PeerBehavior::Event(event) => {
                    Connection::write_success_return_code(&mut send)
                        .await
                        .expect("GET_EVENT success");
                    Connection::write_message(&mut send, &GetEventResponse(Some(event)))
                        .await
                        .expect("event response");
                    send.finish().expect("finish event response");
                }
            }
        }
        RpcId::GET_EVENT_CONTENT => {
            GetEventContentRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
                .expect("decode GET_EVENT_CONTENT");
            content_request_received.notify_one();
            let content = match behavior {
                PeerBehavior::Content(content) => Some(content),
                PeerBehavior::GatedContent(content, gate) => {
                    gate.notified().await;
                    Some(content)
                }
                PeerBehavior::HangContent => std::future::pending().await,
                _ => None,
            };
            Connection::write_success_return_code(&mut send)
                .await
                .expect("GET_EVENT_CONTENT success");
            Connection::write_message(&mut send, &GetEventContentResponse(content.is_some()))
                .await
                .expect("missing content response");
            if let Some(content) = content {
                Connection::write_bao_content(
                    &mut send,
                    content.as_slice(),
                    content.compute_content_hash(),
                )
                .await
                .expect("content response");
            }
            send.finish().expect("finish missing content response");
        }
        // Model an old server: unknown ancestor requests close their streams
        // without a response code, leaving the connection available to fallback.
        RpcId::GET_ANCESTORS => {}
        _ => panic!("unexpected RPC {rpc_id}"),
    }
}

mod holder_tests;

async fn run_test_peer(peer: TestPeer) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            incoming = peer.endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let behavior = peer.behavior.clone();
                let events = peer.event_request_received.clone();
                let contents = peer.content_request_received.clone();
                connections.spawn(async move {
                    let connection = incoming.accept().unwrap().await.unwrap();
                    let mut requests = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            stream = connection.accept_bi() => {
                                let Ok((send, recv)) = stream else { break };
                                requests.spawn(handle_test_peer_rpc(send, recv, behavior.clone(), events.clone(), contents.clone()));
                            },
                            Some(result) = requests.join_next(), if !requests.is_empty() => { result.unwrap(); }
                        }
                    }
                    requests.shutdown().await;
                });
            },
            Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); }
        }
    }
}

fn test_event(secret: RostraIdSecretKey, marker: u8) -> rostra_core::event::VerifiedEvent {
    let content = EventContentRaw::new(vec![marker]);
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&content)
        .build()
        .signed_by(secret);
    rostra_core::event::VerifiedEvent::verify_signed(secret.id(), signed)
        .expect("self-signed event")
}

async fn make_test_network(
    behaviors: Vec<PeerBehavior>,
) -> (
    Arc<Client>,
    Vec<rostra_core::id::RostraId>,
    Vec<Arc<Notify>>,
    Vec<Arc<Notify>>,
    Vec<tokio::task::JoinHandle<()>>,
) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let mut peers = Vec::with_capacity(behaviors.len());
    let mut node_ids = Vec::with_capacity(behaviors.len());
    let mut event_notifications = Vec::with_capacity(behaviors.len());
    let mut content_notifications = Vec::with_capacity(behaviors.len());
    let mut servers = Vec::with_capacity(behaviors.len());

    for behavior in behaviors {
        let id = RostraIdSecretKey::generate().id();
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("peer endpoint");
        lookup.add_endpoint_info(endpoint.addr());
        node_ids.push(IrohNodeId::from_bytes(*endpoint.id().as_bytes()));
        let event_request_received = Arc::new(Notify::new());
        let content_request_received = Arc::new(Notify::new());
        servers.push(tokio::spawn(run_test_peer(TestPeer {
            endpoint,
            behavior,
            event_request_received: Arc::clone(&event_request_received),
            content_request_received: Arc::clone(&content_request_received),
        })));
        peers.push(id);
        event_notifications.push(event_request_received);
        content_notifications.push(content_request_received);
    }

    let local_secret = RostraIdSecretKey::generate();
    let local_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("local endpoint");
    // No default DHT/relay traffic, even if a regression reaches discovery.
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut pkarr = pkarr::Client::builder();
    pkarr
        .no_default_network()
        .request_timeout(Duration::from_millis(100))
        .relays(&[format!("http://{}", relay.local_addr().unwrap())])
        .unwrap()
        .reqwest_client(
            reqwest_13::Client::builder()
                .no_proxy()
                .tls_certs_only([])
                .timeout(Duration::from_millis(100))
                .build()
                .unwrap(),
        );
    let pkarr =
        crate::PkarrClient::build(pkarr, std::num::NonZeroUsize::new(32).unwrap(), 30, 30).unwrap();
    let local = Client::builder(local_secret.id())
        .db(Database::new_in_memory(local_secret.id())
            .await
            .expect("database"))
        .iroh_endpoint(local_endpoint)
        .pkarr_client(Arc::new(pkarr))
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("local client");

    for (peer_id, node_id) in peers.iter().copied().zip(node_ids) {
        local
            .db()
            .insert_id_node(peer_id, node_id, rostra_core::Timestamp::now())
            .await;
    }

    (
        local,
        peers,
        event_notifications,
        content_notifications,
        servers,
    )
}

async fn abort_servers(servers: Vec<tokio::task::JoinHandle<()>>) {
    for server in &servers {
        server.abort();
    }
    for server in servers {
        let result = server.await;
        assert!(
            result.is_ok() || result.unwrap_err().is_cancelled(),
            "test peer failed"
        );
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn expired_first_batch_allows_fifth_peer_to_supply_event() {
    let author_secret = RostraIdSecretKey::generate();
    let expected = test_event(author_secret, 1);
    let expected_id = expected.event_id.to_short();
    let (local, peers, event_notifications, _content_notifications, servers) =
        make_test_network(vec![
            PeerBehavior::HangEvent,
            PeerBehavior::HangEvent,
            PeerBehavior::HangEvent,
            PeerBehavior::HangEvent,
            PeerBehavior::Event(expected.into()),
        ])
        .await;

    let race_local = Arc::clone(&local);
    let race_peers = peers.clone();
    let race = tokio::spawn(async move {
        ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE)
            .get_event_from_peers(
                &race_local.networking,
                &race_peers,
                author_secret.id(),
                expected_id,
            )
            .await
    });
    for notification in &event_notifications[..4] {
        tokio::time::timeout(Duration::from_secs(5), notification.notified())
            .await
            .expect("one of the first four peers received GET_EVENT");
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            event_notifications[4].notified()
        )
        .await
        .is_err(),
        "the fifth candidate must wait while four attempts occupy the race"
    );
    let result = tokio::time::timeout(Duration::from_secs(5), race)
        .await
        .expect("race completes after first batch expires")
        .expect("race task succeeds")
        .expect("fifth peer supplies event");

    assert_eq!(result.event_id.to_short(), expected_id);
    tokio::time::timeout(Duration::from_secs(1), event_notifications[4].notified())
        .await
        .expect("fifth peer received GET_EVENT");
    abort_servers(servers).await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn stalled_peer_and_negative_responses_complete_with_none() {
    let author_secret = RostraIdSecretKey::generate();
    let requested = test_event(author_secret, 2);
    let (local, peers, _event_notifications, _content_notifications, servers) =
        make_test_network(vec![
            PeerBehavior::HangEvent,
            PeerBehavior::Missing,
            PeerBehavior::Missing,
            PeerBehavior::Missing,
            PeerBehavior::Missing,
        ])
        .await;

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE).get_event_from_peers(
            &local.networking,
            &peers,
            author_secret.id(),
            requested.event_id.to_short(),
        ),
    )
    .await
    .expect("negative race completes after stalled attempt expires");

    assert!(result.is_none());
    abort_servers(servers).await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn responsive_peer_result_is_preserved_with_hanging_candidate() {
    let author_secret = RostraIdSecretKey::generate();
    let expected = test_event(author_secret, 3);
    let expected_id = expected.event_id.to_short();
    let (local, peers, _event_notifications, _content_notifications, servers) =
        make_test_network(vec![
            PeerBehavior::HangEvent,
            PeerBehavior::Event(expected.into()),
        ])
        .await;

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE).get_event_from_peers(
            &local.networking,
            &peers,
            author_secret.id(),
            expected_id,
        ),
    )
    .await
    .expect("responsive peer wins without waiting for the hanging peer")
    .expect("responsive peer supplies event");

    assert_eq!(result.event_id.to_short(), expected_id);
    abort_servers(servers).await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn preliminary_ping_stalls_share_the_complete_attempt_deadline() {
    let author_secret = RostraIdSecretKey::generate();
    let expected = test_event(author_secret, 4);
    let expected_id = expected.event_id.to_short();
    let (local, peers, event_notifications, _content_notifications, servers) =
        make_test_network(vec![
            PeerBehavior::HangPing,
            PeerBehavior::HangPing,
            PeerBehavior::HangPing,
            PeerBehavior::HangPing,
            PeerBehavior::Event(expected.into()),
        ])
        .await;

    let race_local = Arc::clone(&local);
    let race_peers = peers.clone();
    let race = tokio::spawn(async move {
        ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE)
            .get_event_from_peers(
                &race_local.networking,
                &race_peers,
                author_secret.id(),
                expected_id,
            )
            .await
    });
    for notification in &event_notifications[..4] {
        tokio::time::timeout(Duration::from_secs(5), notification.notified())
            .await
            .expect("one of the first four peers received PING");
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            event_notifications[4].notified()
        )
        .await
        .is_err(),
        "the fifth candidate must wait while four pings occupy the race"
    );
    let result = tokio::time::timeout(Duration::from_secs(5), race)
        .await
        .expect("connection-stage stalls expire")
        .expect("race task succeeds")
        .expect("later peer supplies event");

    assert_eq!(result.event_id.to_short(), expected_id);
    tokio::time::timeout(Duration::from_secs(1), event_notifications[4].notified())
        .await
        .expect("later peer received GET_EVENT");
    abort_servers(servers).await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn wrong_author_and_event_responses_remain_rejected() {
    let author_secret = RostraIdSecretKey::generate();
    let requested = test_event(author_secret, 5);
    let requested_id = requested.event_id.to_short();
    let wrong_author = test_event(RostraIdSecretKey::generate(), 5);
    let wrong_event = test_event(author_secret, 6);
    for invalid in [wrong_author, wrong_event] {
        let (local, peers, _event_notifications, _content_notifications, servers) =
            make_test_network(vec![PeerBehavior::Event(invalid.into())]).await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE).get_event_from_peers(
                &local.networking,
                &peers,
                author_secret.id(),
                requested_id,
            ),
        )
        .await
        .expect("invalid response is rejected without stalling");
        assert!(result.is_none());
        abort_servers(servers).await;
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn missing_parent_timeout_allows_child_payload_and_later_work_to_progress() {
    let author_secret = RostraIdSecretKey::generate();
    let parent = test_event(author_secret, 7);
    let child_content = EventContentRaw::new(vec![8]);
    let child_signed = Event::builder_raw_content()
        .author(author_secret.id())
        .kind(EventKind::NULL)
        .parent_prev(parent.event_id.to_short())
        .content(&child_content)
        .build()
        .signed_by(author_secret);
    let child = rostra_core::event::VerifiedEvent::verify_signed(author_secret.id(), child_signed)
        .expect("self-signed child");
    let later = test_event(author_secret, 9);
    let (local, peers, event_notifications, content_notifications, servers) =
        make_test_network(vec![PeerBehavior::HangEvent, PeerBehavior::Missing]).await;
    local
        .db()
        .try_process_event(&child)
        .await
        .expect("store retained child envelope");
    local
        .db()
        .try_process_event(&later)
        .await
        .expect("store later retained envelope");
    let cache = ConnectionCache::with_peer_operation_deadline(TEST_PEER_DEADLINE);

    let downloaded = tokio::time::timeout(
        Duration::from_secs(5),
        crate::util::rpc::download_events_from_child(
            author_secret.id(),
            child.event_id.to_short(),
            &local.networking,
            &cache,
            &peers,
            local.db(),
        ),
    )
    .await
    .expect("missing-parent traversal completes after peer timeout")
    .expect("traversal has no database failure");
    assert!(!downloaded);
    tokio::time::timeout(Duration::from_secs(1), event_notifications[0].notified())
        .await
        .expect("stalled peer received the missing-parent GET_EVENT");
    tokio::time::timeout(Duration::from_secs(1), content_notifications[1].notified())
        .await
        .expect("traversal progressed to the retained child's payload");

    tokio::time::timeout(
        Duration::from_secs(5),
        crate::util::rpc::download_events_from_child(
            author_secret.id(),
            later.event_id.to_short(),
            &local.networking,
            &cache,
            &peers,
            local.db(),
        ),
    )
    .await
    .expect("later due work is not stranded")
    .expect("later traversal has no database failure");
    tokio::time::timeout(Duration::from_secs(1), content_notifications[1].notified())
        .await
        .expect("later retained payload was attempted");

    abort_servers(servers).await;
}

#[tokio::test]
async fn pending_cell_survives_cleanup_and_coalesces_initialization() {
    let cache = ConnectionCache::new();
    let cell = Arc::new(OnceCell::new());
    let mut cells = HashMap::from([(1, Arc::clone(&cell))]);
    let initialization_count = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();

    let first_cell = Arc::clone(&cell);
    let first_count = Arc::clone(&initialization_count);
    let first = tokio::spawn(async move {
        *first_cell
            .get_or_try_init(|| async move {
                first_count.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok::<_, Infallible>(7)
            })
            .await
            .unwrap()
    });
    started_rx.await.unwrap();

    assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 0);
    let retained = Arc::clone(cells.get(&1).unwrap());
    assert!(Arc::ptr_eq(&cell, &retained));

    let second_count = Arc::clone(&initialization_count);
    let second = retained.get_or_try_init(|| async move {
        second_count.fetch_add(1, Ordering::SeqCst);
        Ok::<_, Infallible>(99)
    });
    tokio::pin!(second);
    assert!(futures::poll!(second.as_mut()).is_pending());

    release_tx.send(()).unwrap();
    assert_eq!(first.await.unwrap(), 7);
    assert_eq!(*second.await.unwrap(), 7);
    assert_eq!(initialization_count.load(Ordering::SeqCst), 1);
    assert_eq!(cells.get(&1).unwrap().get(), Some(&7));
}

#[tokio::test]
async fn cancelled_initializer_leaves_empty_cell_for_later_cleanup() {
    let cache = ConnectionCache::new();
    let cell = Arc::new(OnceCell::<()>::new());
    let initializing_cell = Arc::clone(&cell);
    let mut cells = HashMap::from([(1, cell)]);
    let (started_tx, started_rx) = oneshot::channel();

    let initializer = tokio::spawn(async move {
        let _ = initializing_cell
            .get_or_try_init(|| async move {
                started_tx.send(()).unwrap();
                std::future::pending::<Result<(), Infallible>>().await
            })
            .await;
    });
    started_rx.await.unwrap();

    assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 0);
    assert!(cells.contains_key(&1));

    initializer.abort();
    assert!(initializer.await.unwrap_err().is_cancelled());
    let retrying_cell = Arc::clone(cells.get(&1).unwrap());
    assert!(Arc::ptr_eq(cells.get(&1).unwrap(), &retrying_cell));
    assert_eq!(
        retrying_cell
            .get_or_try_init(|| async { Err::<(), _>(()) })
            .await,
        Err(())
    );
    assert!(retrying_cell.get().is_none());
    drop(retrying_cell);

    for _ in 0..CLEANUP_INTERVAL - 1 {
        assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 0);
        assert!(cells.contains_key(&1));
    }
    assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 1);
    assert!(!cells.contains_key(&1));
}

#[test]
fn empty_cell_remains_while_shared_and_is_removed_after_release() {
    let cache = ConnectionCache::new();
    let cell = Arc::new(OnceCell::<()>::new());
    let waiter = Arc::clone(&cell);
    let mut cells = HashMap::from([(1, cell)]);

    assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 0);
    assert!(cells.contains_key(&1));

    drop(waiter);
    for _ in 0..CLEANUP_INTERVAL - 1 {
        assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 0);
        assert!(cells.contains_key(&1));
    }
    assert_eq!(cache.maybe_cleanup_cells(&mut cells, |_| true), 1);
    assert!(!cells.contains_key(&1));
}

#[test]
fn initialized_open_cell_survives_and_closed_cell_is_removed() {
    let cache = ConnectionCache::new();
    let open = Arc::new(AtomicBool::new(true));
    let open_cell = Arc::new(OnceCell::new());
    open_cell.set(Arc::clone(&open)).unwrap();
    let external_cell_owner = Arc::clone(&open_cell);
    let external_connection = Arc::clone(open_cell.get().unwrap());
    let mut cells = HashMap::from([(1, open_cell)]);

    assert_eq!(
        cache.maybe_cleanup_cells(&mut cells, |connection| connection.load(Ordering::SeqCst)),
        0
    );
    assert!(cells.contains_key(&1));

    open.store(false, Ordering::SeqCst);
    for _ in 0..CLEANUP_INTERVAL - 1 {
        cache.maybe_cleanup_cells(&mut cells, |connection| connection.load(Ordering::SeqCst));
    }
    assert_eq!(
        cache.maybe_cleanup_cells(&mut cells, |connection| connection.load(Ordering::SeqCst)),
        1
    );
    assert!(!cells.contains_key(&1));
    assert!(!external_cell_owner.get().unwrap().load(Ordering::SeqCst));
    assert!(!external_connection.load(Ordering::SeqCst));
}

#[test]
fn completion_after_shared_detection_remains_discoverable() {
    let cache = ConnectionCache::new();
    let cell = Arc::new(OnceCell::new());
    let completing_owner = Arc::clone(&cell);
    let mut cells = HashMap::from([(1, cell)]);
    let (complete_tx, complete_rx) = std::sync::mpsc::channel();
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let completion = std::thread::spawn(move || {
        complete_rx.recv().unwrap();
        completing_owner.set(true).unwrap();
        drop(completing_owner);
        completed_tx.send(()).unwrap();
    });

    assert_eq!(
        cache.maybe_cleanup_cells_after_shared(
            &mut cells,
            |is_open| *is_open,
            |_| {
                complete_tx.send(()).unwrap();
                completed_rx.recv().unwrap();
            }
        ),
        0
    );
    completion.join().unwrap();
    assert_eq!(cells.get(&1).unwrap().get(), Some(&true));
}
