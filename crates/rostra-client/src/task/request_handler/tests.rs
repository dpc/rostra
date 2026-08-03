use std::time::Duration;

use iroh::endpoint::{RecvStream, SendStream, presets};
use rostra_client_db::Database;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::Connection;
use rostra_p2p::connection::{FeedEventRequest, FeedEventResponse};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;

use super::{
    InboundAdmission, MAX_CONCURRENT_INBOUND_RPCS, MAX_INBOUND_CONNECTIONS, ORDINARY_RPC_TIMEOUT,
    RESERVED_ORDINARY_INBOUND_RPCS,
};
use crate::Client;

#[test]
fn client_wide_admission_stays_bounded_and_recovers_after_release() {
    let admission = InboundAdmission::new();

    let connection_permits = (0..MAX_INBOUND_CONNECTIONS)
        .map(|_| {
            admission
                .try_admit_connection()
                .expect("connection within the global budget")
        })
        .collect::<Vec<_>>();
    assert!(
        admission.try_admit_connection().is_err(),
        "an excess connection must be rejected instead of creating a task"
    );

    let shared_rpc_count = MAX_CONCURRENT_INBOUND_RPCS - RESERVED_ORDINARY_INBOUND_RPCS;
    let mut rpc_permits = (0..shared_rpc_count)
        .map(|_| {
            admission
                .try_admit_rpc(true)
                .expect("long wait within the global budget")
        })
        .collect::<Vec<_>>();
    assert!(
        admission.try_admit_rpc(true).is_err(),
        "an excess persistent RPC must be rejected instead of creating a task"
    );

    let ordinary_rpc = admission
        .try_admit_rpc(false)
        .expect("reserved capacity admits ordinary traffic while long polls are saturated");
    drop(ordinary_rpc);

    rpc_permits.pop();
    let replacement_long_poll = admission
        .try_admit_rpc(true)
        .expect("persistent traffic resumes after a long wait releases its permit");
    assert_eq!(admission.shared_rpcs.available_permits(), 0);
    drop(replacement_long_poll);

    drop(connection_permits);
    assert_eq!(
        admission.connections.available_permits(),
        MAX_INBOUND_CONNECTIONS
    );
    assert_eq!(
        admission.shared_rpcs.available_permits(),
        shared_rpc_count - rpc_permits.len()
    );
    assert_eq!(
        admission.reserved_ordinary_rpcs.available_permits(),
        RESERVED_ORDINARY_INBOUND_RPCS
    );
}

async fn request_handler_fixture() -> (
    RostraIdSecretKey,
    std::sync::Arc<Client>,
    iroh::Endpoint,
    iroh::endpoint::Connection,
) {
    let secret = RostraIdSecretKey::from_bytes([73; 32]);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let server_endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("server endpoint");
    let server_id = server_endpoint.id();
    lookup.add_endpoint_info(server_endpoint.addr());
    let server = Client::builder(secret.id())
        .db(Database::new_in_memory(secret.id())
            .await
            .expect("in-memory database"))
        .iroh_endpoint(server_endpoint)
        .start_background_tasks(false)
        .build()
        .await
        .expect("server client");
    let caller_endpoint = iroh::Endpoint::builder(presets::Minimal)
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
    (secret, server, caller_endpoint, connection)
}

async fn write_feed_event_header(send: &mut SendStream, request: &FeedEventRequest) {
    let payload = bincode::encode_to_vec(request, rostra_core::bincode::STD_BINCODE_CONFIG)
        .expect("encode request");
    let mut header = vec![0, 1];
    header.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("request length fits u32")
            .to_be_bytes(),
    );
    header.extend_from_slice(&payload);
    send.write_all(&header).await.expect("write request header");
}

async fn read_return_code(recv: &mut RecvStream) -> u8 {
    let mut code = [0];
    recv.read_exact(&mut code).await.expect("read return code");
    code[0]
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_finite_rpc_times_out_and_connection_recovers() {
    let (secret, _server, caller_endpoint, raw_connection) = request_handler_fixture().await;
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&EventContentRaw::new(vec![1]))
        .build()
        .signed_by(secret);
    let (mut send, mut recv) = raw_connection.open_bi().await.expect("open RPC stream");
    write_feed_event_header(&mut send, &FeedEventRequest(event)).await;
    assert_eq!(read_return_code(&mut recv).await, 0);
    Connection::read_message::<1024, FeedEventResponse>(&mut recv)
        .await
        .expect("feed-event readiness response");

    tokio::time::sleep(ORDINARY_RPC_TIMEOUT + Duration::from_millis(50)).await;
    let mut response = [0];
    let closed = recv.read(&mut response).await;
    assert!(
        closed.is_err() || closed.ok() == Some(None),
        "the stalled request body must be canceled after the finite RPC deadline"
    );

    let connection = Connection::from(raw_connection);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), connection.ping(41))
            .await
            .expect("ordinary RPC after timeout")
            .expect("ping response"),
        41
    );
    caller_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admitted_long_poll_survives_finite_deadline_and_allows_ordinary_rpc() {
    let (secret, server, caller_endpoint, raw_connection) = request_handler_fixture().await;
    let content = EventContentRaw::new(vec![2]);
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&content)
        .build()
        .signed_by(secret);
    let verified =
        VerifiedEvent::verify_signed(secret.id(), signed).expect("self-signed test event");
    let first = VerifiedEventContent::verify(verified, content).expect("matching test content");
    let first_id = first.event.event_id.to_short();
    server.db().process_event_with_content(&first).await;
    let connection = Connection::from(raw_connection);
    let mut wait = Box::pin(connection.wait_head_update(first_id));

    assert!(
        tokio::time::timeout(ORDINARY_RPC_TIMEOUT + Duration::from_millis(50), &mut wait)
            .await
            .is_err(),
        "the explicit long-poll whitelist must not use the finite RPC deadline"
    );
    assert_eq!(
        connection.ping(42).await.expect("reserved ordinary RPC"),
        42
    );
    drop(wait);
    caller_endpoint.close().await;
}
