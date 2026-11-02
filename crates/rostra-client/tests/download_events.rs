use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rostra_client::Client;
use rostra_client_db::Database;
use rostra_core::event::content_kind::IrohNodeId;
use rostra_core::event::{
    Event, EventExt as _, EventKind, PersonasTagsSelector, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_p2p::connection::{
    Connection, GetEventContentRequest, GetEventContentResponse, GetEventRequest, GetEventResponse,
    MAX_REQUEST_SIZE, PingRequest, PingResponse, RpcId, RpcMessage as _,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tokio::sync::{Notify, Semaphore};

#[path = "download_events/ancestor_pages.rs"]
mod ancestor_pages;

#[derive(Clone, Debug, Default)]
struct RecordedRequests {
    ancestors: Arc<Mutex<Vec<ShortEventId>>>,
    events: Arc<Mutex<Vec<ShortEventId>>>,
    contents: Arc<Mutex<Vec<ShortEventId>>>,
    changed: Arc<Notify>,
}

impl RecordedRequests {
    fn events(&self) -> Vec<ShortEventId> {
        self.events.lock().expect("event request lock").clone()
    }

    fn contents(&self) -> Vec<ShortEventId> {
        self.contents.lock().expect("content request lock").clone()
    }

    async fn wait_for_contents(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let changed = self.changed.notified();
                if self.contents().len() >= count {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("content requests made progress");
    }
}

struct RecordedPeer {
    endpoint_id: iroh::PublicKey,
    requests: RecordedRequests,
    task: tokio::task::JoinHandle<()>,
}

impl RecordedPeer {
    async fn start(
        lookup: iroh::address_lookup::memory::MemoryLookup,
        contents: impl IntoIterator<Item = VerifiedEventContent>,
    ) -> Self {
        Self::start_gated(lookup, contents, HashMap::new()).await
    }

    async fn start_gated(
        lookup: iroh::address_lookup::memory::MemoryLookup,
        contents: impl IntoIterator<Item = VerifiedEventContent>,
        gates: HashMap<ShortEventId, Arc<Semaphore>>,
    ) -> Self {
        Self::start_config(lookup, contents, gates, None).await
    }

    async fn start_config(
        lookup: iroh::address_lookup::memory::MemoryLookup,
        contents: impl IntoIterator<Item = VerifiedEventContent>,
        gates: HashMap<ShortEventId, Arc<Semaphore>>,
        pages: Option<HashMap<ShortEventId, rostra_p2p::connection::GetAncestorsResponse>>,
    ) -> Self {
        let pages = Arc::new(pages);
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("recording peer endpoint");
        let endpoint_id = endpoint.id();
        lookup.add_endpoint_info(endpoint.addr());
        let contents: Arc<HashMap<_, _>> = Arc::new(
            contents
                .into_iter()
                .map(|content| (content.event_id().to_short(), content))
                .collect(),
        );
        let requests = RecordedRequests::default();
        let task_requests = requests.clone();
        let task = tokio::spawn(async move {
            let gates = Arc::new(gates);
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let Some(incoming) = endpoint.accept().await else {
                    return;
                };
                let connection = incoming
                    .accept()
                    .expect("accept recording peer")
                    .await
                    .expect("recording peer connection");
                let contents = contents.clone();
                let requests = task_requests.clone();
                let gates = gates.clone();
                let pages = pages.clone();
                connections.spawn(async move {
                    let mut handlers = tokio::task::JoinSet::new();
                    loop {
                        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                            return;
                        };
                        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
                            .await
                            .expect("recording peer request");
                        let contents = contents.clone();
                        let requests = requests.clone();
                        let gates = gates.clone();
                        let pages = pages.clone();
                        handlers.spawn(async move {
                            if rpc_id == RpcId::GET_ANCESTORS {
                                let req =
                                    rostra_p2p::connection::GetAncestorsRequest::decode_whole::<
                                        MAX_REQUEST_SIZE,
                                    >(&request)
                                    .expect("ancestor request");
                                requests.ancestors.lock().unwrap().push(req.root);
                                // Match production old-server behavior: drop unknown streams
                                // without writing a response code.
                                let Some(pages) = pages.as_ref() else { return };
                                Connection::write_success_return_code(&mut send)
                                    .await
                                    .unwrap();
                                let empty = rostra_p2p::connection::GetAncestorsResponse(
                                    [None; rostra_p2p::connection::ANCESTOR_PAGE_EVENTS],
                                );
                                Connection::write_message(
                                    &mut send,
                                    pages.get(&req.root).unwrap_or(&empty),
                                )
                                .await
                                .unwrap();
                                return;
                            }
                            Connection::write_success_return_code(&mut send)
                                .await
                                .expect("recording peer return code");
                            match rpc_id {
                                RpcId::PING => {
                                    let request =
                                        PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
                                            .expect("decode ping");
                                    Connection::write_message(&mut send, &PingResponse(request.0))
                                        .await
                                        .expect("write ping response");
                                }
                                RpcId::GET_EVENT => {
                                    let event_id =
                                        GetEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
                                            .expect("decode event request")
                                            .0;
                                    requests
                                        .events
                                        .lock()
                                        .expect("event request lock")
                                        .push(event_id);
                                    let event = contents.get(&event_id).map(|content| {
                                        rostra_core::event::SignedEvent::from(content.event)
                                    });
                                    Connection::write_message(&mut send, &GetEventResponse(event))
                                        .await
                                        .expect("write event response");
                                }
                                RpcId::GET_EVENT_CONTENT => {
                                    let event_id = GetEventContentRequest::decode_whole::<
                                        MAX_REQUEST_SIZE,
                                    >(&request)
                                    .expect("decode content request")
                                    .0;
                                    requests
                                        .contents
                                        .lock()
                                        .expect("content request lock")
                                        .push(event_id);
                                    requests.changed.notify_one();
                                    if let Some(gate) = gates.get(&event_id) {
                                        gate.acquire().await.expect("content gate").forget();
                                    }
                                    let content = contents
                                        .get(&event_id)
                                        .and_then(|content| content.content.as_ref());
                                    Connection::write_message(
                                        &mut send,
                                        &GetEventContentResponse(content.is_some()),
                                    )
                                    .await
                                    .expect("write content response");
                                    if let Some(content) = content {
                                        let event =
                                            &contents.get(&event_id).expect("matching event").event;
                                        Connection::write_bao_content(
                                            &mut send,
                                            content.as_ref(),
                                            event.content_hash(),
                                        )
                                        .await
                                        .expect("write event content");
                                    }
                                }
                                _ => panic!("unexpected RPC {rpc_id}"),
                            }
                        });
                    }
                });
            }
        });
        Self {
            endpoint_id,
            requests,
            task,
        }
    }
}

impl Drop for RecordedPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn build_test_event(
    id_secret: RostraIdSecretKey,
    parent_prev: impl Into<Option<ShortEventId>>,
) -> (VerifiedEvent, VerifiedEventContent) {
    build_test_event_with_text(id_secret, parent_prev, "test content")
}

fn build_test_event_with_text(
    id_secret: RostraIdSecretKey,
    parent_prev: impl Into<Option<ShortEventId>>,
    text: &str,
) -> (VerifiedEvent, VerifiedEventContent) {
    build_test_event_with_parents(id_secret, parent_prev, None, text)
}

fn build_test_event_with_parents(
    id_secret: RostraIdSecretKey,
    parent_prev: impl Into<Option<ShortEventId>>,
    parent_aux: impl Into<Option<ShortEventId>>,
    text: &str,
) -> (VerifiedEvent, VerifiedEventContent) {
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;

    let parent = parent_prev.into();
    let post = content_kind::SocialPost::new(text.to_string(), None, Default::default());
    let content = post.serialize_cbor().expect("valid cbor");
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent)
        .maybe_parent_aux(parent_aux.into())
        .content(&content)
        .build();

    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
    let verified_content =
        VerifiedEventContent::verify(verified_event, content).expect("Valid content");
    (verified_event, verified_content)
}

async fn build_recorded_client(
    client_id: RostraId,
    lookup: iroh::address_lookup::memory::MemoryLookup,
    peer_id: RostraId,
    peer_endpoint_id: iroh::PublicKey,
) -> Arc<Client> {
    build_recorded_client_with_account(client_id, lookup, peer_id, peer_endpoint_id, None).await
}

async fn build_recorded_client_with_account(
    client_id: RostraId,
    lookup: iroh::address_lookup::memory::MemoryLookup,
    peer_id: RostraId,
    peer_endpoint_id: iroh::PublicKey,
    account: Option<rostra_client_db::PayloadAccount>,
) -> Arc<Client> {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("recorded client endpoint");
    let mut db = Database::new_in_memory(client_id)
        .await
        .expect("recorded client database");
    if let Some(account) = account {
        db.attach_payload_account(&account).unwrap();
        while !db
            .rebuild_payload_accounting(std::num::NonZeroUsize::new(100).unwrap())
            .await
            .unwrap()
            .ready
        {}
    }
    db.insert_id_node(
        peer_id,
        IrohNodeId::from_bytes(*peer_endpoint_id.as_bytes()),
        Timestamp::now(),
    )
    .await;
    Client::builder(client_id)
        .db(db)
        .iroh_endpoint(endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("recorded client")
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_download_events_from_child() -> BoxedErrorResult<()> {
    let secret_a = RostraIdSecretKey::generate();
    let id_a = secret_a.id();
    let secret_b = RostraIdSecretKey::generate();
    let id_b = secret_b.id();

    // Create a shared MemoryLookup for iroh address discovery between the two
    // endpoints
    let mem_lookup = iroh::address_lookup::memory::MemoryLookup::new();

    // Create endpoint A (the server)
    let ep_a = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(mem_lookup.clone())
        .bind()
        .await
        .boxed()?;

    // Get A's address info before creating the client (which moves the
    // endpoint)
    let ep_a_pub_id = ep_a.id();
    let ep_a_addr = ep_a.addr();

    // Add A's address to the shared lookup so B can discover it
    mem_lookup.add_endpoint_info(ep_a_addr);

    // Create endpoint B (the client)
    let ep_b = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(mem_lookup.clone())
        .bind()
        .await
        .boxed()?;

    // Build client A: with request handler (server), no background tasks
    let client_a = Client::builder(id_a)
        .db(Database::new_in_memory(id_a).await?)
        .iroh_endpoint(ep_a)
        .start_background_tasks(false)
        .build()
        .await?;

    // Build client B: no request handler, no background tasks
    let client_b = Client::builder(id_b)
        .db(Database::new_in_memory(id_b).await?)
        .iroh_endpoint(ep_b)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await?;

    // Get DB references through the clients
    let db_a = client_a.db();
    let db_b = client_b.db();

    // Register A's iroh node address in B's database so B can connect to A
    let iroh_node_id = IrohNodeId::from_bytes(*ep_a_pub_id.as_bytes());
    db_b.insert_id_node(id_a, iroh_node_id, Timestamp::now())
        .await;

    // Create a chain of 5 events in client A's database:
    // event_0 (genesis) -> event_1 -> event_2 -> event_3 -> event_4 (head)
    let num_events = 5;
    let mut events = Vec::new();
    let mut parent: Option<ShortEventId> = None;

    for _ in 0..num_events {
        let (event, content) = build_test_event(secret_a, parent);
        db_a.process_event_with_content(&content).await;
        parent = Some(event.event_id.to_short());
        events.push(event);
    }

    let head_event = events.last().expect("Must have events");
    let head_id = head_event.event_id.to_short();

    // Verify the events exist in A's DB but not in B's
    for event in &events {
        let eid = event.event_id.to_short();
        assert!(db_a.has_event(eid).await, "Event {eid} should exist in A");
        assert!(
            !db_b.has_event(eid).await,
            "Event {eid} should not exist in B yet"
        );
    }

    // Client B explicitly synchronizes all ancestors from the discovered head.
    let peers = vec![id_a, id_b];

    let downloaded = client_b
        .sync_event_from_peers(id_a, head_id, &peers)
        .await
        .expect("event synchronization should not fail");

    assert!(downloaded, "Should have downloaded new events");

    // Verify ALL events now exist in B's database
    for event in &events {
        let eid = event.event_id.to_short();
        assert!(
            db_b.has_event(eid).await,
            "Event {eid} should now exist in B after download_events_from_child"
        );
    }

    // Verify content was downloaded for all events too
    for event in &events {
        let eid = event.event_id.to_short();
        let content = db_b.get_event_content(eid).await;
        assert!(
            content.is_some(),
            "Event content for {eid} should exist in B"
        );
    }

    // An envelope retained before explicit synchronization still reports the
    // later content-only materialization as progress.
    let (content_only_event, content_only) =
        build_test_event_with_text(secret_a, None, "content only");
    db_a.process_event_with_content(&content_only).await;
    db_b.try_process_event(&content_only_event).await?;
    let content_only_id = content_only_event.event_id.to_short();
    assert!(db_b.get_event_content(content_only_id).await.is_none());
    assert!(
        client_b
            .sync_event_from_peers(id_a, content_only_id, &peers)
            .await?
    );
    assert!(db_b.get_event_content(content_only_id).await.is_some());
    assert!(
        !client_b
            .sync_event_from_peers(id_a, content_only_id, &peers)
            .await?
    );

    // A retained envelope whose payload is unavailable makes no storage
    // progress.
    let (unavailable_event, _) = build_test_event_with_text(secret_a, None, "unavailable payload");
    db_a.try_process_event(&unavailable_event).await?;
    db_b.try_process_event(&unavailable_event).await?;
    assert!(
        !client_b
            .sync_event_from_peers(id_a, unavailable_event.event_id.to_short(), &peers)
            .await?
    );

    // A fresh envelope remains progress even when its payload is unavailable.
    let (fresh_envelope, _) = build_test_event_with_text(secret_a, None, "fresh envelope");
    db_a.try_process_event(&fresh_envelope).await?;
    let fresh_id = fresh_envelope.event_id.to_short();
    assert!(
        client_b
            .sync_event_from_peers(id_a, fresh_id, &peers)
            .await?
    );
    assert!(db_b.has_event(fresh_id).await);
    assert!(db_b.get_event_content(fresh_id).await.is_none());
    assert!(
        !client_b
            .sync_event_from_peers(id_a, fresh_id, &peers)
            .await?
    );

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn cross_author_local_parents_do_not_leave_requested_graph() -> BoxedErrorResult<()> {
    let secret_a = RostraIdSecretKey::generate();
    let id_a = secret_a.id();
    let secret_b = RostraIdSecretKey::generate();
    let id_b = secret_b.id();
    let client_id = RostraIdSecretKey::generate().id();

    let (prev_ancestor, prev_ancestor_content) =
        build_test_event_with_text(secret_b, None, "previous ancestor");
    let (aux_ancestor, aux_ancestor_content) =
        build_test_event_with_text(secret_b, None, "auxiliary ancestor");
    let (cross_prev, cross_prev_content) = build_test_event_with_text(
        secret_b,
        prev_ancestor.event_id.to_short(),
        "cross-author previous parent",
    );
    let (cross_aux, cross_aux_content) = build_test_event_with_text(
        secret_b,
        aux_ancestor.event_id.to_short(),
        "cross-author auxiliary parent",
    );
    let (head, head_content) = build_test_event_with_parents(
        secret_a,
        cross_prev.event_id.to_short(),
        cross_aux.event_id.to_short(),
        "requested-author head",
    );

    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(
        lookup.clone(),
        [
            head_content,
            cross_prev_content,
            cross_aux_content,
            prev_ancestor_content,
            aux_ancestor_content,
        ],
    )
    .await;
    let client = build_recorded_client(client_id, lookup, id_a, peer.endpoint_id).await;
    client.db().try_process_event(&head).await?;
    client.db().try_process_event(&cross_prev).await?;
    client.db().try_process_event(&cross_aux).await?;
    let missing_before = client.db().get_missing_events_for_id(id_a).await;
    let b_heads_before = client.db().get_heads(id_b).await;
    let cross_prev_state_before = client
        .db()
        .get_event_content_state(cross_prev.event_id.to_short())
        .await;
    let cross_aux_state_before = client
        .db()
        .get_event_content_state(cross_aux.event_id.to_short())
        .await;

    assert!(
        client
            .sync_event_from_peers(id_a, head.event_id.to_short(), &[id_a])
            .await?
    );

    assert!(
        client
            .db()
            .get_event_content(head.event_id.to_short())
            .await
            .is_some()
    );
    assert!(
        client
            .db()
            .get_event_content(cross_prev.event_id.to_short())
            .await
            .is_none()
    );
    assert!(
        client
            .db()
            .get_event_content(cross_aux.event_id.to_short())
            .await
            .is_none()
    );
    assert!(
        !client
            .db()
            .has_event(prev_ancestor.event_id.to_short())
            .await
    );
    assert!(
        !client
            .db()
            .has_event(aux_ancestor.event_id.to_short())
            .await
    );

    let event_requests = peer.requests.events();
    assert!(event_requests.contains(&cross_prev.event_id.to_short()));
    assert!(event_requests.contains(&cross_aux.event_id.to_short()));
    assert!(!event_requests.contains(&prev_ancestor.event_id.to_short()));
    assert!(!event_requests.contains(&aux_ancestor.event_id.to_short()));
    assert_eq!(
        client.db().get_missing_events_for_id(id_a).await,
        missing_before,
        "cross-author local rows must not resolve requested-author parents"
    );
    assert_eq!(
        client.db().get_heads(id_b).await,
        b_heads_before,
        "requested-author traversal must not change the other author's graph"
    );
    assert_eq!(
        client
            .db()
            .get_event_content_state(cross_prev.event_id.to_short())
            .await,
        cross_prev_state_before
    );
    assert_eq!(
        client
            .db()
            .get_event_content_state(cross_aux.event_id.to_short())
            .await,
        cross_aux_state_before
    );
    assert_eq!(
        peer.requests.contents(),
        vec![head.event_id.to_short()],
        "only the requested author's eligible head may fetch payload"
    );
    assert_eq!(
        client
            .db()
            .get_event(cross_prev.event_id.to_short())
            .await
            .expect("cross-author previous row remains")
            .author(),
        id_b
    );
    assert_eq!(
        client
            .db()
            .get_event(cross_aux.event_id.to_short())
            .await
            .expect("cross-author auxiliary row remains")
            .author(),
        id_b
    );

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn mismatched_local_starting_head_uses_verified_remote_fallback() -> BoxedErrorResult<()> {
    let id_a = RostraIdSecretKey::generate().id();
    let secret_b = RostraIdSecretKey::generate();
    let client_id = RostraIdSecretKey::generate().id();
    let (ancestor_b, ancestor_content_b) =
        build_test_event_with_text(secret_b, None, "wrong-author ancestor");
    let (event_b, content_b) = build_test_event_with_text(
        secret_b,
        ancestor_b.event_id.to_short(),
        "wrong-author head",
    );
    let event_id = event_b.event_id.to_short();
    let ancestor_id = ancestor_b.event_id.to_short();

    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(lookup.clone(), [content_b, ancestor_content_b]).await;
    let client = build_recorded_client(client_id, lookup, id_a, peer.endpoint_id).await;
    client.db().try_process_event(&event_b).await?;

    assert!(
        !client
            .sync_event_from_peers(id_a, event_id, &[id_a])
            .await?
    );
    assert_eq!(peer.requests.events(), vec![event_id]);
    assert!(peer.requests.contents().is_empty());
    assert!(client.db().get_event_content(event_id).await.is_none());
    assert!(!client.db().has_event(ancestor_id).await);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn content_fetch_rejects_missing_and_satisfied_cross_author_local_rows()
-> BoxedErrorResult<()> {
    let id_a = RostraIdSecretKey::generate().id();
    let secret_b = RostraIdSecretKey::generate();
    let client_id = RostraIdSecretKey::generate().id();
    let (missing_event, missing_content) =
        build_test_event_with_text(secret_b, None, "missing cross-author content");
    let (satisfied_event, satisfied_content) =
        build_test_event_with_text(secret_b, None, "satisfied cross-author content");
    let missing_id = missing_event.event_id.to_short();
    let satisfied_id = satisfied_event.event_id.to_short();

    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(
        lookup.clone(),
        [missing_content.clone(), satisfied_content.clone()],
    )
    .await;
    let client = build_recorded_client(client_id, lookup, id_a, peer.endpoint_id).await;
    client.db().try_process_event(&missing_event).await?;
    client
        .db()
        .try_process_event_with_content(&satisfied_content)
        .await?;
    let mut followers = BTreeMap::new();

    assert!(
        !client
            .fetch_event_content(id_a, missing_id, &mut followers)
            .await?
    );
    assert!(
        !client
            .fetch_event_content(id_a, satisfied_id, &mut followers)
            .await?
    );
    assert_eq!(peer.requests.events(), vec![missing_id, satisfied_id]);
    assert!(peer.requests.contents().is_empty());
    assert!(client.db().get_event_content(missing_id).await.is_none());
    assert!(client.db().get_event_content(satisfied_id).await.is_some());

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn content_fetch_keeps_same_author_local_and_remote_paths() -> BoxedErrorResult<()> {
    let secret_a = RostraIdSecretKey::generate();
    let id_a = secret_a.id();
    let client_id = RostraIdSecretKey::generate().id();
    let (local_event, local_content) =
        build_test_event_with_text(secret_a, None, "same-author local envelope");
    let (remote_event, remote_content) =
        build_test_event_with_text(secret_a, None, "same-author remote envelope");
    let local_id = local_event.event_id.to_short();
    let remote_id = remote_event.event_id.to_short();

    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(lookup.clone(), [local_content, remote_content]).await;
    let client = build_recorded_client(client_id, lookup, id_a, peer.endpoint_id).await;
    client.db().try_process_event(&local_event).await?;
    let mut followers = BTreeMap::new();

    assert!(
        client
            .fetch_event_content(id_a, local_id, &mut followers)
            .await?
    );
    assert!(
        client
            .fetch_event_content(id_a, remote_id, &mut followers)
            .await?
    );
    assert_eq!(peer.requests.events(), vec![remote_id]);
    assert_eq!(peer.requests.contents(), vec![local_id, remote_id]);
    assert!(client.db().get_event_content(local_id).await.is_some());
    assert!(client.db().get_event_content(remote_id).await.is_some());

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn retained_same_author_head_still_traverses_remote_parent() -> BoxedErrorResult<()> {
    let secret_a = RostraIdSecretKey::generate();
    let id_a = secret_a.id();
    let client_id = RostraIdSecretKey::generate().id();
    let (parent, parent_content) =
        build_test_event_with_text(secret_a, None, "same-author remote parent");
    let (head, head_content) = build_test_event_with_text(
        secret_a,
        parent.event_id.to_short(),
        "same-author retained head",
    );
    let parent_id = parent.event_id.to_short();
    let head_id = head.event_id.to_short();

    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(lookup.clone(), [head_content, parent_content]).await;
    let client = build_recorded_client(client_id, lookup, id_a, peer.endpoint_id).await;
    client.db().try_process_event(&head).await?;

    assert!(client.sync_event_from_peers(id_a, head_id, &[id_a]).await?);
    assert_eq!(peer.requests.events(), vec![parent_id]);
    let mut requested = peer.requests.contents();
    requested.sort();
    let mut expected = vec![parent_id, head_id];
    expected.sort();
    assert_eq!(requested, expected);
    assert!(client.db().get_event_content(head_id).await.is_some());
    assert!(client.db().get_event_content(parent_id).await.is_some());

    Ok(())
}

fn gated_history(
    secret: RostraIdSecretKey,
    count: usize,
    text_len: usize,
) -> (
    Vec<VerifiedEventContent>,
    Vec<ShortEventId>,
    HashMap<ShortEventId, Arc<Semaphore>>,
) {
    let mut history = Vec::new();
    let mut ids = Vec::new();
    let mut gates = HashMap::new();
    for n in 0..count {
        let (event, content) = build_test_event_with_text(
            secret,
            ids.last().copied(),
            &format!("{n}:{}", "x".repeat(text_len)),
        );
        let id = event.event_id.to_short();
        ids.push(id);
        history.push(content);
        gates.insert(id, Arc::new(Semaphore::new(0)));
    }
    (history, ids, gates)
}

async fn wait_for_materialized(client: &Client, id: ShortEventId) {
    let mut changed = client.db().new_content_subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.db().get_event_content(id).await.is_none() {
            match changed.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("content notifications closed before materialization");
                }
            }
        }
    })
    .await
    .expect("released payload materialized");
}

fn limited_payload_account(id: RostraId, buffers: usize) -> rostra_client_db::PayloadAccount {
    use std::num::{NonZeroU64, NonZeroUsize};

    use rostra_client_db::{
        PayloadAccount, PayloadAdmissionConfig, PayloadAdmissionLimits, PayloadRetentionConfig,
        PayloadRuntimeLimits,
    };

    PayloadAccount::configured(
        id,
        PayloadRetentionConfig::Enforce {
            policy: rostra_core::retention::RetentionPolicy::new(1, 1, 0, 0, 1, 0).unwrap(),
            admission: PayloadAdmissionConfig::new(PayloadAdmissionLimits {
                database_bytes: NonZeroU64::new(100_000_000).unwrap(),
                author_bytes: NonZeroU64::new(100_000_000).unwrap(),
                overrides: Default::default(),
                in_flight_count: NonZeroUsize::new(buffers).unwrap(),
                in_flight_bytes: NonZeroU64::new(100_000_000).unwrap(),
            })
            .unwrap(),
            limits: PayloadRuntimeLimits {
                operations: NonZeroUsize::new(32).unwrap(),
                bytes: 100_000_000,
                gc_bytes: 100_000_000,
                time: Duration::from_millis(20),
            },
        },
    )
    .unwrap()
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn history_overlaps_four_payloads_and_materializes_ready_younger_events()
-> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::generate();
    let (history, ids, gates) = gated_history(secret, 8, 32);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start_gated(lookup.clone(), history, gates.clone()).await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    let sync = {
        let client = client.clone();
        let head = *ids.last().unwrap();
        tokio::spawn(async move {
            client
                .sync_event_from_peers(secret.id(), head, &[secret.id()])
                .await
        })
    };

    // The serial implementation cannot reach this barrier. No payload response
    // is available, yet four independent iroh streams have arrived.
    peer.requests.wait_for_contents(4).await;
    assert_eq!(peer.requests.events().len(), 8);
    let mut first_wave = peer.requests.contents();
    first_wave.sort();
    let mut oldest = ids[..4].to_vec();
    oldest.sort();
    assert_eq!(first_wave, oldest, "oldest candidates launch first");
    for id in ids[1..4].iter().rev() {
        gates[id].add_permits(1);
        wait_for_materialized(&client, *id).await;
    }
    assert!(client.db().get_event_content(ids[0]).await.is_none());
    assert_eq!(peer.requests.contents().len(), 4);
    gates[&ids[0]].add_permits(1);
    peer.requests.wait_for_contents(8).await;
    for id in ids[4..].iter().rev() {
        gates[id].add_permits(1);
    }
    assert!(tokio::time::timeout(Duration::from_secs(10), sync).await???);
    for id in &ids {
        assert!(client.db().get_event_content(*id).await.is_some());
    }
    assert_eq!(peer.requests.contents().len(), 8);
    assert!(
        client
            .db()
            .get_missing_events_for_id(secret.id())
            .await
            .is_empty()
    );
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn history_byte_budget_serializes_large_payloads_without_admission() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::generate();
    let (history, ids, gates) = gated_history(secret, 3, 2 * 1024 * 1024 + 1);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start_gated(lookup.clone(), history, gates.clone()).await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    let sync = {
        let client = client.clone();
        let head = *ids.last().unwrap();
        tokio::spawn(async move {
            client
                .sync_event_from_peers(secret.id(), head, &[secret.id()])
                .await
        })
    };
    for (n, id) in ids.iter().enumerate() {
        peer.requests.wait_for_contents(n + 1).await;
        assert_eq!(peer.requests.contents(), ids[..=n]);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                peer.requests.wait_for_contents(n + 2)
            )
            .await
            .is_err(),
            "byte budget must prevent a second large in-flight payload"
        );
        gates[id].add_permits(1);
        wait_for_materialized(&client, *id).await;
    }
    assert!(tokio::time::timeout(Duration::from_secs(10), sync).await???);
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn history_retries_sibling_capacity_refusals_and_releases_guards_on_cancel()
-> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::generate();
    let (history, ids, gates) = gated_history(secret, 4, 32);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start_gated(lookup.clone(), history, gates.clone()).await;
    let client_id = RostraIdSecretKey::generate().id();
    let client = build_recorded_client_with_account(
        client_id,
        lookup,
        secret.id(),
        peer.endpoint_id,
        Some(limited_payload_account(client_id, 2)),
    )
    .await;
    let head = *ids.last().unwrap();
    let sync = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .sync_event_from_peers(secret.id(), head, &[secret.id()])
                .await
        })
    };
    peer.requests.wait_for_contents(1).await;
    assert_eq!(client.db().payload_admission_usage().buffers, 2);
    sync.abort();
    assert!(sync.await.unwrap_err().is_cancelled());
    assert_eq!(client.db().payload_admission_usage().buffers, 0);
    assert_eq!(client.db().payload_admission_usage().acquisitions, 0);

    // Retry the same durable Missing work with only enough capacity for one
    // read/conversion pair. Refused siblings must not be treated as peer misses.
    for gate in gates.values() {
        gate.add_permits(2);
    }
    assert!(
        tokio::time::timeout(
            Duration::from_secs(10),
            client.sync_event_from_peers(secret.id(), head, &[secret.id()]),
        )
        .await??
    );
    for id in ids {
        assert!(client.db().get_event_content(id).await.is_some());
    }
    assert_eq!(client.db().payload_admission_usage().buffers, 0);
    assert_eq!(client.db().payload_admission_usage().acquisitions, 0);
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn history_missing_payload_does_not_discard_ready_siblings() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::generate();
    let (mut history, ids, _) = gated_history(secret, 4, 32);
    history[0].content = None;
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(lookup.clone(), history.clone()).await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    for content in &history {
        client.db().try_process_event(&content.event).await?;
    }
    assert!(
        client
            .sync_event_from_peers(secret.id(), ids[3], &[secret.id()])
            .await?
    );
    assert!(client.db().get_event_content(ids[0]).await.is_none());
    for id in &ids[1..] {
        assert!(client.db().get_event_content(*id).await.is_some());
    }
    assert_eq!(peer.requests.contents().len(), 4);
    assert!(peer.requests.events().is_empty());
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn history_persistent_capacity_pause_returns_without_network_reads() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::generate();
    let (history, ids, _) = gated_history(secret, 4, 32);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start(lookup.clone(), history).await;
    let client_id = RostraIdSecretKey::generate().id();
    // A read requires two buffers. No sibling completion can make this
    // configuration fit, and retry must not spin or bypass admission.
    let client = build_recorded_client_with_account(
        client_id,
        lookup,
        secret.id(),
        peer.endpoint_id,
        Some(limited_payload_account(client_id, 1)),
    )
    .await;
    // Enforced shared-store preparation has its own existing 30-second
    // pressure wait. Allow that wait plus the single post-drain retry.
    let result = tokio::time::timeout(
        Duration::from_secs(70),
        client.sync_event_from_peers(secret.id(), ids[3], &[secret.id()]),
    )
    .await?;
    assert!(matches!(
        result,
        Err(rostra_client_db::DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(peer.requests.contents().is_empty());
    for id in ids {
        assert!(client.db().has_event(id).await);
        assert!(client.db().get_event_content(id).await.is_none());
    }
    assert_eq!(client.db().payload_admission_usage().buffers, 0);
    assert_eq!(client.db().payload_admission_usage().acquisitions, 0);
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn history_inflight_duplicate_materialization_does_not_report_false_progress()
-> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::generate();
    let (history, ids, gates) = gated_history(secret, 4, 32);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let peer = RecordedPeer::start_gated(lookup.clone(), history.clone(), gates.clone()).await;
    let client = build_recorded_client(
        RostraIdSecretKey::generate().id(),
        lookup,
        secret.id(),
        peer.endpoint_id,
    )
    .await;
    for content in &history {
        client.db().try_process_event(&content.event).await?;
    }
    let sync = {
        let client = client.clone();
        let head = ids[3];
        tokio::spawn(async move {
            client
                .sync_event_from_peers(secret.id(), head, &[secret.id()])
                .await
        })
    };
    peer.requests.wait_for_contents(4).await;
    // Simulate another replication path winning after admission but before
    // these reads complete. Transactional ingestion must recheck lifecycle.
    for content in &history {
        client.db().try_process_event_content(content).await?;
    }
    for gate in gates.values() {
        gate.add_permits(1);
    }
    assert!(!tokio::time::timeout(Duration::from_secs(10), sync).await???);
    assert!(peer.requests.events().is_empty());
    Ok(())
}

/// Test that when client B follows client A *after* both clients are already
/// running with background tasks, B's polling task picks up A as a followee
/// and syncs A's events.
///
/// This exercises the full follow → watch notification → poll → sync flow.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_follow_while_running_syncs_events() -> BoxedErrorResult<()> {
    let secret_a = RostraIdSecretKey::generate();
    let id_a = secret_a.id();
    let secret_b = RostraIdSecretKey::generate();
    let id_b = secret_b.id();

    let mem_lookup = iroh::address_lookup::memory::MemoryLookup::new();

    // Create endpoint A (serves events)
    let ep_a = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(mem_lookup.clone())
        .bind()
        .await
        .boxed()?;

    let ep_a_pub_id = ep_a.id();
    let ep_a_addr = ep_a.addr();
    mem_lookup.add_endpoint_info(ep_a_addr);

    // Create endpoint B (follows A)
    let ep_b = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(mem_lookup.clone())
        .bind()
        .await
        .boxed()?;

    let ep_b_pub_id = ep_b.id();
    let ep_b_addr = ep_b.addr();
    mem_lookup.add_endpoint_info(ep_b_addr);

    // Build client A: request handler ON (serves RPC), background tasks ON
    let client_a = Client::builder(id_a)
        .db(Database::new_in_memory(id_a).await?)
        .iroh_endpoint(ep_a)
        .secret(secret_a)
        .start_background_tasks(true)
        .build()
        .await?;

    // Build client B: request handler ON, background tasks ON
    // (poll_followee_head_updates will run and watch for followee changes)
    let client_b = Client::builder(id_b)
        .db(Database::new_in_memory(id_b).await?)
        .iroh_endpoint(ep_b)
        .secret(secret_b)
        .start_background_tasks(true)
        .build()
        .await?;

    let db_a = client_a.db();
    let db_b = client_b.db();

    // Register each other's iroh node addresses
    let iroh_node_a = IrohNodeId::from_bytes(*ep_a_pub_id.as_bytes());
    db_b.insert_id_node(id_a, iroh_node_a, Timestamp::now())
        .await;
    let iroh_node_b = IrohNodeId::from_bytes(*ep_b_pub_id.as_bytes());
    db_a.insert_id_node(id_b, iroh_node_b, Timestamp::now())
        .await;

    // Get A's current head (from the node-announcement created during
    // unlock_active)
    let a_current_head = db_a.get_self_current_head().await;

    // A publishes a post (before B follows A), chained from the current head
    let (event_before_follow, content_before) = build_test_event(secret_a, a_current_head);
    db_a.process_event_with_content(&content_before).await;
    let pre_follow_head = event_before_follow.event_id.to_short();

    // B follows A (while both clients are already running)
    client_b
        .follow(secret_b, id_a, PersonasTagsSelector::default())
        .await
        .boxed()?;

    // A publishes another post (after B followed)
    let (event_after_follow, content_after) = build_test_event(secret_a, pre_follow_head);
    db_a.process_event_with_content(&content_after).await;
    let post_follow_event_id = event_after_follow.event_id.to_short();

    // Wait for B's background tasks to sync the event from A.
    // The poll_followee_head_updates task should detect A as a new followee
    // via the watch channel, connect to A, and fetch the head event.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let synced = loop {
        if db_b.has_event(post_follow_event_id).await {
            break true;
        }
        if deadline < tokio::time::Instant::now() {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert!(
        synced,
        "Client B should have synced A's post via background polling after following A"
    );

    Ok(())
}
