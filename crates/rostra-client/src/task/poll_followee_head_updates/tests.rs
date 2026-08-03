use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::Database;
use rostra_core::event::{
    Event, EventContentRaw, EventKind, IrohNodeId, SignedEvent, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_p2p::connection::{
    Connection, GetEventRequest, GetEventResponse, MAX_REQUEST_SIZE, PingRequest, PingResponse,
    RpcId, RpcMessage as _, WaitHeadUpdateRequest, WaitHeadUpdateResponse,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{RwLock, mpsc};

use super::{PeerBackoffState, PollFolloweeHeadUpdates};
use crate::Client;

fn build_event(
    id_secret: RostraIdSecretKey,
    content_byte: u8,
    parent_prev: Option<ShortEventId>,
) -> VerifiedEventContent {
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
    VerifiedEventContent::verify(event, content).expect("matching test content")
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn polling_reuses_returned_remote_head_as_wait_cursor() {
    let remote_secret = RostraIdSecretKey::generate();
    let remote_id = remote_secret.id();
    let remote_head = build_event(remote_secret, 1, None);
    let remote_head_id = remote_head.event.event_id.to_short();
    let local_descendant = build_event(remote_secret, 2, Some(remote_head_id));
    let local_descendant_id = local_descendant.event.event_id.to_short();

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

    let client_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("client endpoint");
    let client_secret = RostraIdSecretKey::generate();
    let client = Client::builder(client_secret.id())
        .db(Database::new_in_memory(client_secret.id())
            .await
            .expect("in-memory database"))
        .iroh_endpoint(client_endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("client");
    let db = client.db().clone();
    db.process_event_with_content(&remote_head).await;
    db.process_event_with_content(&local_descendant).await;
    db.insert_id_node(
        remote_id,
        IrohNodeId::from_bytes(*server_id.as_bytes()),
        Timestamp::now(),
    )
    .await;

    let (wait_requests_tx, mut wait_requests_rx) = mpsc::channel(2);
    let server = tokio::spawn(async move {
        let incoming = server_endpoint.accept().await.expect("incoming connection");
        let connection = incoming
            .accept()
            .expect("accept incoming")
            .await
            .expect("connection");

        let (mut send, mut recv) = connection.accept_bi().await.expect("ping stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("ping request");
        assert_eq!(rpc_id, RpcId::PING);
        assert_eq!(
            PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
                .expect("decode ping")
                .0,
            0
        );
        Connection::write_success_return_code(&mut send)
            .await
            .expect("ping success");
        Connection::write_message(&mut send, &PingResponse(0))
            .await
            .expect("ping response");

        let (mut send, mut recv) = connection.accept_bi().await.expect("first wait stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("first wait request");
        assert_eq!(rpc_id, RpcId::WAIT_HEAD_UPDATE);
        let requested_head = WaitHeadUpdateRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
            .expect("decode first wait")
            .0;
        wait_requests_tx
            .send(requested_head)
            .await
            .expect("first wait receiver");
        Connection::write_success_return_code(&mut send)
            .await
            .expect("first wait success");
        Connection::write_message(&mut send, &WaitHeadUpdateResponse(remote_head_id))
            .await
            .expect("first wait response");

        let (mut send, mut recv) = connection.accept_bi().await.expect("event stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("event request");
        assert_eq!(rpc_id, RpcId::GET_EVENT);
        assert_eq!(
            GetEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
                .expect("decode event request")
                .0,
            remote_head_id
        );
        Connection::write_success_return_code(&mut send)
            .await
            .expect("event success");
        Connection::write_message(
            &mut send,
            &GetEventResponse(Some(SignedEvent {
                event: remote_head.event.event,
                sig: remote_head.event.sig,
            })),
        )
        .await
        .expect("event response");

        let (mut send, mut recv) = connection.accept_bi().await.expect("second wait stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("second wait request");
        assert_eq!(rpc_id, RpcId::WAIT_HEAD_UPDATE);
        let requested_head = WaitHeadUpdateRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
            .expect("decode second wait")
            .0;
        wait_requests_tx
            .send(requested_head)
            .await
            .expect("second wait receiver");
        Connection::write_success_return_code(&mut send)
            .await
            .expect("second wait success");

        std::future::pending::<()>().await;
    });

    let polling = tokio::spawn(PollFolloweeHeadUpdates::poll_followee(
        client.networking().clone(),
        client.connection_cache().clone(),
        db,
        remote_id,
        Arc::new(RwLock::new(HashMap::<_, PeerBackoffState>::new())),
    ));

    let first_request = tokio::time::timeout(Duration::from_secs(2), wait_requests_rx.recv())
        .await
        .expect("first wait request timeout")
        .expect("first wait request");
    assert_eq!(first_request, local_descendant_id);

    let second_request = tokio::time::timeout(Duration::from_secs(2), wait_requests_rx.recv())
        .await
        .expect("second wait request timeout")
        .expect("second wait request");
    assert_eq!(second_request, remote_head_id);

    polling.abort();
    server.abort();
}
