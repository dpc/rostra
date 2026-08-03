use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::Database;
use rostra_core::event::content_kind::{EventContentKind as _, Follow, PersonaSelector};
use rostra_core::event::{
    Event, EventContentRaw, EventKind, IrohNodeId, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::connection::{
    Connection, GetHeadRequest, MAX_REQUEST_SIZE, PingRequest, PingResponse, RpcId, RpcMessage as _,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::Notify;

use super::{SyncCycleOutcome, WotHeadSync};
use crate::Client;

fn event_content(secret: RostraIdSecretKey) -> VerifiedEventContent {
    let content = EventContentRaw::new(vec![7]);
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&content)
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), signed).expect("self-signed event");
    VerifiedEventContent::verify(event, content).expect("matching event content")
}

fn follow_content(
    secret: RostraIdSecretKey,
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
        .author(secret.id())
        .kind(EventKind::FOLLOW)
        .content(&content)
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), signed).expect("self-signed follow");
    VerifiedEventContent::verify(event, content).expect("matching follow content")
}

async fn hanging_head_server(endpoint: iroh::Endpoint, get_head_received: Arc<Notify>) {
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

    let (_send, mut recv) = connection.accept_bi().await.expect("head stream");
    let (rpc_id, request) = Connection::read_request_raw(&mut recv)
        .await
        .expect("head request");
    assert_eq!(rpc_id, RpcId::GET_HEAD);
    GetHeadRequest::decode_whole::<MAX_REQUEST_SIZE>(&request).expect("decode GET_HEAD");
    get_head_received.notify_one();
    std::future::pending::<()>().await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn hanging_peer_does_not_block_later_wot_head_sync() {
    let target_secret = RostraIdSecretKey::generate();
    let target_id = target_secret.id();
    let hanging_secret = RostraIdSecretKey::generate();
    let hanging_id = hanging_secret.id();
    let local_secret = RostraIdSecretKey::generate();
    let local_id = local_secret.id();
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
    let get_head_received = Arc::new(Notify::new());
    let hanging_server = tokio::spawn(hanging_head_server(
        hanging_endpoint,
        get_head_received.clone(),
    ));

    let target_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("target endpoint");
    let target_node_id = target_endpoint.id();
    lookup.add_endpoint_info(target_endpoint.addr());
    let target = Client::builder(target_id)
        .db(Database::new_in_memory(target_id)
            .await
            .expect("target database"))
        .iroh_endpoint(target_endpoint)
        .start_background_tasks(false)
        .build()
        .await
        .expect("target client");
    let target_event = event_content(target_secret);
    let target_head = target_event.event.event_id.to_short();
    target.db().process_event_with_content(&target_event).await;

    let local_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("local endpoint");
    let local = Client::builder(local_id)
        .db(Database::new_in_memory(local_id)
            .await
            .expect("local database"))
        .iroh_endpoint(local_endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("local client");
    let db = local.db();
    db.insert_id_node(
        hanging_id,
        IrohNodeId::from_bytes(*hanging_node_id.as_bytes()),
        rostra_core::Timestamp::now(),
    )
    .await;
    db.insert_id_node(
        target_id,
        IrohNodeId::from_bytes(*target_node_id.as_bytes()),
        rostra_core::Timestamp::now(),
    )
    .await;
    db.process_event_with_content(&follow_content(hanging_secret, local_id))
        .await;
    db.process_event_with_content(&follow_content(local_secret, target_id))
        .await;

    let sync = WotHeadSync::new(&local);
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        sync.sync_cycle_with_deadlines(Duration::from_secs(1), Duration::from_secs(8)),
    )
    .await
    .expect("cycle continues after a hanging peer")
    .expect("database ingestion succeeds");
    assert_eq!(outcome, SyncCycleOutcome::Complete);
    tokio::time::timeout(Duration::from_secs(1), get_head_received.notified())
        .await
        .expect("first peer received GET_HEAD");
    assert!(
        db.has_event(target_head).await,
        "later responsive peer supplied the missing head"
    );
    assert_eq!(
        sync.sync_cycle_with_deadlines(Duration::from_secs(1), Duration::ZERO)
            .await
            .expect("a cycle deadline is not a database failure"),
        SyncCycleOutcome::TimedOut,
        "the worker-level cycle budget stops an incomplete sweep"
    );

    hanging_server.abort();
    assert!(
        hanging_server
            .await
            .expect_err("server is cancelled")
            .is_cancelled()
    );
    drop(local);
    drop(target);
}
