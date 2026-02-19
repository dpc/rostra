use std::sync::Arc;
use std::time::Duration;

use rostra_p2p::connection::{
    Connection, MAX_REQUEST_SIZE, PingRequest, PingResponse, RpcId, RpcMessage as _,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{Notify, Semaphore};

/// Verify that RPCs on a single connection are handled concurrently
/// when the server spawns each handler as a separate task (with a
/// semaphore for concurrency control).
///
/// The test simulates a server that blocks on one RPC and verifies
/// that a second RPC on the same connection completes independently.
#[tokio::test]
async fn test_concurrent_rpcs_on_same_connection() {
    let mem_lookup = iroh::address_lookup::memory::MemoryLookup::new();

    // Server endpoint
    let ep_server = iroh::Endpoint::empty_builder(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(mem_lookup.clone())
        .bind()
        .await
        .unwrap();

    let server_id = ep_server.id();
    let server_addr = ep_server.addr();
    mem_lookup.add_endpoint_info(server_addr);

    // Client endpoint
    let ep_client = iroh::Endpoint::empty_builder(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(mem_lookup.clone())
        .bind()
        .await
        .unwrap();

    // Shared signal: the blocking handler waits on this before responding
    let unblock = Arc::new(Notify::new());
    let unblock_server = unblock.clone();

    // Spawn server: accept one connection, handle RPCs concurrently
    let server_handle = tokio::spawn(async move {
        let incoming = ep_server.accept().await.unwrap();
        let conn = incoming.accept().unwrap().await.unwrap();

        let semaphore = Arc::new(Semaphore::new(32));

        // Accept RPCs until the connection closes
        loop {
            let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                break;
            };
            let Ok((rpc_id, req_msg)) = Connection::read_request_raw(&mut recv).await else {
                break;
            };

            let unblock = unblock_server.clone();
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore never closed");

            tokio::spawn(async move {
                if rpc_id == RpcId::PING {
                    let req = PingRequest::decode_whole::<MAX_REQUEST_SIZE>(&req_msg).unwrap();

                    // If the ping value is 0xB10C, simulate a
                    // blocking handler (like WAIT_FOLLOWERS_NEW_HEADS)
                    if req.0 == 0xB10C {
                        unblock.notified().await;
                    }

                    Connection::write_success_return_code(&mut send)
                        .await
                        .unwrap();
                    Connection::write_message(&mut send, &PingResponse(req.0))
                        .await
                        .unwrap();
                }
                drop(permit);
            });
        }
    });

    // Client connects
    let iroh_conn = ep_client
        .connect(server_id, ROSTRA_P2P_V0_ALPN)
        .await
        .unwrap();
    let conn = Connection::from(iroh_conn);

    // Send a "blocking" RPC (will hang until we signal unblock)
    let conn_blocking = conn.clone();
    let blocking_rpc = tokio::spawn(async move { conn_blocking.ping(0xB10C).await });

    // Give the blocking RPC time to be accepted by the server
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send a normal ping — this must complete even though the
    // blocking RPC is still in progress on the same connection.
    let normal_result = tokio::time::timeout(Duration::from_secs(5), conn.ping(42)).await;

    assert!(
        normal_result.is_ok(),
        "Normal ping should complete while blocking RPC is in progress"
    );
    assert_eq!(normal_result.unwrap().unwrap(), 42);

    // Now unblock the first RPC and verify it also completes
    unblock.notify_one();
    let blocking_result = tokio::time::timeout(Duration::from_secs(5), blocking_rpc).await;
    assert!(blocking_result.is_ok(), "Blocking RPC should complete");
    assert_eq!(blocking_result.unwrap().unwrap().unwrap(), 0xB10C);

    server_handle.abort();
}
