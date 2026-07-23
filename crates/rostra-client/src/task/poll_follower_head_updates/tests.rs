use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::Database;
use rostra_core::event::{Event, EventContentRaw, EventKind, SignedEvent, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_p2p::connection::{
    Connection, MAX_REQUEST_SIZE, RpcId, RpcMessage as _, WaitFollowersNewHeadsRequest,
    WaitFollowersNewHeadsResponse,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{RwLock, oneshot};

use super::{PeerBackoffState, PollFollowerHeadUpdates, SharedBackoffState};

fn signed_event(secret: RostraIdSecretKey, content_byte: u8) -> SignedEvent {
    let content = EventContentRaw::new(vec![content_byte]);
    Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&content)
        .build()
        .signed_by(secret)
}

fn verified_event_with_author(
    secret: RostraIdSecretKey,
    author: RostraId,
    content_byte: u8,
) -> VerifiedEvent {
    let mut event = VerifiedEvent::verify_signed(secret.id(), signed_event(secret, content_byte))
        .expect("fixture signature");
    event.event.author = author;
    event.event_id = event.event.compute_id();
    event
}

async fn connection_returning(
    response: WaitFollowersNewHeadsResponse,
) -> (
    Connection,
    iroh::Endpoint,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let server_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("server endpoint");
    let server_id = server_endpoint.id();
    lookup.add_endpoint_info(server_endpoint.addr());

    let (release_server, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let incoming = server_endpoint.accept().await.expect("incoming connection");
        let connection = incoming
            .accept()
            .expect("accept connection")
            .await
            .expect("complete handshake");
        let (mut send, mut recv) = connection.accept_bi().await.expect("RPC stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("RPC request");
        assert_eq!(rpc_id, RpcId::WAIT_FOLLOWERS_NEW_HEADS);
        WaitFollowersNewHeadsRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
            .expect("WAIT_FOLLOWERS_NEW_HEADS request");

        Connection::write_success_return_code(&mut send)
            .await
            .expect("success return code");
        Connection::write_message(&mut send, &response)
            .await
            .expect("RPC response");
        send.finish().expect("finish RPC response");
        released.await.expect("release test server");
        server_endpoint.close().await;
    });

    let caller_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("caller endpoint");
    let connection = caller_endpoint
        .connect(server_id, ROSTRA_P2P_V0_ALPN)
        .await
        .expect("connect to server");

    (
        Connection::from(connection),
        caller_endpoint,
        release_server,
        server,
    )
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn rpc_ingests_event_when_claimed_and_signed_authors_match() {
    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let event = signed_event(self_secret, 1);
    let event_id = event.compute_short_id();
    let db = Database::new_in_memory(self_id)
        .await
        .expect("in-memory database");
    let wot = db.self_wot_subscribe();
    let (connection, caller_endpoint, release_server, server) =
        connection_returning(WaitFollowersNewHeadsResponse {
            author: self_id,
            event,
        })
        .await;

    let event = PollFollowerHeadUpdates::poll_once(&connection, self_id, &wot)
        .await
        .expect("matching self-authored response is admitted")
        .expect("self-authored response is in the web of trust");
    db.try_process_event(&event)
        .await
        .expect("admitted response is stored");
    release_server.send(()).expect("release server");
    server.await.expect("test server");
    drop(connection);
    caller_endpoint.close().await;

    assert!(db.has_event(event_id).await);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn rpc_rejects_trusted_claim_with_event_signed_by_another_author() {
    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let attacker_secret = RostraIdSecretKey::generate();
    let attacker_event = signed_event(attacker_secret, 2);
    let attacker_event_id = attacker_event.compute_short_id();
    let db = Database::new_in_memory(self_id)
        .await
        .expect("in-memory database");
    let wot = db.self_wot_subscribe();
    let (connection, caller_endpoint, release_server, server) =
        connection_returning(WaitFollowersNewHeadsResponse {
            author: self_id,
            event: attacker_event,
        })
        .await;

    let error = PollFollowerHeadUpdates::poll_once(&connection, self_id, &wot)
        .await
        .expect_err("claimed author must match the signed event author");
    release_server.send(()).expect("release server");
    server.await.expect("test server");
    drop(connection);
    caller_endpoint.close().await;

    assert!(
        error.contains("AuthorMismatch"),
        "unexpected verification error: {error}"
    );
    assert!(!db.has_event(attacker_event_id).await);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn storage_failure_stops_before_resetting_peer_backoff() {
    let first_secret = RostraIdSecretKey::from_bytes([41; 32]);
    let second_secret = RostraIdSecretKey::from_bytes([42; 32]);
    let (prefix, first_rest) = first_secret.id().split();
    let (_, second_rest) = second_secret.id().split();
    let first_author = RostraId::assemble(prefix, first_rest);
    let second_author = RostraId::assemble(prefix, second_rest);
    let first = verified_event_with_author(first_secret, first_author, 3);
    let second = verified_event_with_author(second_secret, second_author, 4);
    let db = Database::new_in_memory(first_author)
        .await
        .expect("in-memory database");
    db.try_process_event(&first)
        .await
        .expect("first identity mapping");

    let peer_id = RostraIdSecretKey::from_bytes([43; 32]).id();
    let backoff_until = tokio::time::Instant::now() + Duration::from_secs(30);
    let backoff_state: SharedBackoffState = Arc::new(RwLock::new(HashMap::from([(
        peer_id,
        PeerBackoffState {
            consecutive_failures: 2,
            backoff_until: Some(backoff_until),
        },
    )])));

    PollFollowerHeadUpdates::finish_successful_poll(&db, peer_id, Some(&second), &backoff_state)
        .await
        .expect_err("identity collision must remain a database failure");

    let state = backoff_state.read().await;
    let peer_state = state.get(&peer_id).expect("peer backoff state");
    assert_eq!(peer_state.consecutive_failures, 2);
    assert_eq!(peer_state.backoff_until, Some(backoff_until));
}
