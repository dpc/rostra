use std::collections::HashMap;
use std::sync::Arc;

use rostra_core::ShortEventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::connection::{
    Connection, GetEventRequest, GetEventResponse, MAX_REQUEST_SIZE, RpcId, RpcMessage as _,
    WaitHeadUpdateRequest, WaitHeadUpdateResponse,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{RwLock, mpsc};

use super::{PeerPollState, PollFolloweeHeadUpdates, SharedPollState};

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

async fn poll_from_local_head(
    connection: &Connection,
    followee_id: rostra_core::id::RostraId,
    local_head: ShortEventId,
    poll_state: &SharedPollState,
) -> Result<(), String> {
    loop {
        PollFolloweeHeadUpdates::poll_remote_head_update(
            connection,
            followee_id,
            local_head,
            poll_state,
        )
        .await?;
    }
}

#[test_log::test(tokio::test(start_paused = true))]
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
    let (wait_requests_tx, mut wait_requests_rx) = mpsc::channel(3);
    let server = tokio::spawn(async move {
        let incoming = server_endpoint.accept().await.expect("incoming connection");
        let connection = incoming
            .accept()
            .expect("accept incoming")
            .await
            .expect("connection");

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
        Connection::write_message(&mut send, &GetEventResponse(None))
            .await
            .expect("event response");

        let (_second_send, mut second_recv) =
            connection.accept_bi().await.expect("second wait stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut second_recv)
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

        let (_send, mut recv) = connection.accept_bi().await.expect("third wait stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("third wait request");
        assert_eq!(rpc_id, RpcId::WAIT_HEAD_UPDATE);
        let requested_head = WaitHeadUpdateRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
            .expect("decode third wait")
            .0;
        wait_requests_tx
            .send(requested_head)
            .await
            .expect("third wait receiver");

        std::future::pending::<()>().await;
    });
    let connection = Connection::from(
        client_endpoint
            .connect(server_id, ROSTRA_P2P_V0_ALPN)
            .await
            .expect("connect to server"),
    );
    let poll_state: SharedPollState = Arc::new(RwLock::new(HashMap::<_, PeerPollState>::new()));
    let polling_connection = connection.clone();
    let polling_state = poll_state.clone();
    let polling = tokio::spawn(async move {
        tokio::time::timeout(
            super::POLL_SLOT_TIMEOUT,
            poll_from_local_head(
                &polling_connection,
                remote_id,
                local_descendant_id,
                &polling_state,
            ),
        )
        .await
        .unwrap_or(Ok(()))
    });

    let first_request = wait_requests_rx.recv().await.expect("first wait request");
    assert_eq!(first_request, local_descendant_id);

    let second_request = wait_requests_rx.recv().await.expect("second wait request");
    assert_eq!(second_request, remote_head_id);

    tokio::time::advance(super::POLL_SLOT_TIMEOUT).await;
    polling
        .await
        .expect("first poll slot task")
        .expect("first poll slot result");

    let polling_connection = connection.clone();
    let polling = tokio::spawn(async move {
        tokio::time::timeout(
            super::POLL_SLOT_TIMEOUT,
            poll_from_local_head(
                &polling_connection,
                remote_id,
                local_descendant_id,
                &poll_state,
            ),
        )
        .await
        .unwrap_or(Ok(()))
    });
    let third_request = wait_requests_rx.recv().await.expect("third wait request");
    assert_eq!(third_request, remote_head_id);

    polling.abort();
    server.abort();
    drop(connection);
    client_endpoint.close().await;
}
