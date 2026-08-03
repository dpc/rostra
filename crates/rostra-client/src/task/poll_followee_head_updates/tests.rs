use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rostra_core::ShortEventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::connection::{
    Connection, GetEventRequest, GetEventResponse, MAX_REQUEST_SIZE, RpcId, RpcMessage as _,
    WaitHeadUpdateRequest, WaitHeadUpdateResponse,
};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use tokio::sync::{RwLock, mpsc, oneshot};

use super::{ActiveFolloweePoll, FolloweePollState, PollFolloweeHeadUpdates, RemoteProgress};

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

#[test_log::test(tokio::test(start_paused = true))]
async fn poll_slots_retain_cursor_and_retry_pending_event_after_cancellation() {
    let remote_secret = RostraIdSecretKey::generate();
    let remote_id = remote_secret.id();
    let remote_head = build_event(remote_secret, 1, None).event;
    let remote_head_id = remote_head.event_id.to_short();
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
    let (requests_tx, mut requests_rx) = mpsc::channel(4);
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
        requests_tx
            .send((rpc_id, requested_head))
            .await
            .expect("request receiver");
        Connection::write_success_return_code(&mut send)
            .await
            .expect("first wait success");
        Connection::write_message(&mut send, &WaitHeadUpdateResponse(remote_head_id))
            .await
            .expect("first wait response");

        for _ in 0..2 {
            let (mut send, mut recv) = connection.accept_bi().await.expect("event stream");
            let (rpc_id, request) = Connection::read_request_raw(&mut recv)
                .await
                .expect("event request");
            assert_eq!(rpc_id, RpcId::GET_EVENT);
            let event_id = GetEventRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
                .expect("decode event request")
                .0;
            requests_tx
                .send((rpc_id, event_id))
                .await
                .expect("request receiver");
            Connection::write_success_return_code(&mut send)
                .await
                .expect("event success");
            Connection::write_message(&mut send, &GetEventResponse(Some(remote_head.into())))
                .await
                .expect("event response");
        }

        let (_send, mut recv) = connection.accept_bi().await.expect("second wait stream");
        let (rpc_id, request) = Connection::read_request_raw(&mut recv)
            .await
            .expect("second wait request");
        assert_eq!(rpc_id, RpcId::WAIT_HEAD_UPDATE);
        let requested_head = WaitHeadUpdateRequest::decode_whole::<MAX_REQUEST_SIZE>(&request)
            .expect("decode second wait")
            .0;
        requests_tx
            .send((rpc_id, requested_head))
            .await
            .expect("request receiver");
        std::future::pending::<()>().await;
    });
    let connection = Connection::from(
        client_endpoint
            .connect(server_id, ROSTRA_P2P_V0_ALPN)
            .await
            .expect("connect to server"),
    );
    let followee_state = Arc::new(RwLock::new(FolloweePollState::default()));
    let persist_attempts = Arc::new(AtomicUsize::new(0));

    let slot_connection = connection.clone();
    let slot_state = followee_state.clone();
    let slot_attempts = persist_attempts.clone();
    let first_slot = tokio::spawn(async move {
        tokio::time::timeout(
            super::POLL_SLOT_TIMEOUT,
            PollFolloweeHeadUpdates::poll_connection_slot(
                &slot_connection,
                remote_id,
                local_descendant_id,
                &slot_state,
                move |_| {
                    slot_attempts.fetch_add(1, Ordering::SeqCst);
                    std::future::pending()
                },
            ),
        )
        .await
    });

    assert_eq!(
        requests_rx.recv().await,
        Some((RpcId::WAIT_HEAD_UPDATE, local_descendant_id))
    );
    assert_eq!(
        requests_rx.recv().await,
        Some((RpcId::GET_EVENT, remote_head_id))
    );
    while persist_attempts.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(super::POLL_SLOT_TIMEOUT).await;
    assert!(first_slot.await.expect("first slot task").is_err());
    {
        let state = followee_state.read().await;
        assert!(matches!(
            state.remote_progress,
            RemoteProgress::Pending(id) if id == remote_head_id
        ));
    }

    let slot_connection = connection.clone();
    let slot_state = followee_state.clone();
    let second_slot = tokio::spawn(async move {
        tokio::time::timeout(
            super::POLL_SLOT_TIMEOUT,
            PollFolloweeHeadUpdates::poll_connection_slot(
                &slot_connection,
                remote_id,
                local_descendant_id,
                &slot_state,
                |_| async { Ok(()) },
            ),
        )
        .await
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(super::MISSING_EVENT_RETRY_DELAY).await;
    assert_eq!(
        requests_rx.recv().await,
        Some((RpcId::GET_EVENT, remote_head_id))
    );
    assert_eq!(
        requests_rx.recv().await,
        Some((RpcId::WAIT_HEAD_UPDATE, remote_head_id))
    );
    {
        let state = followee_state.read().await;
        assert!(matches!(
            state.remote_progress,
            RemoteProgress::Persisted(id) if id == remote_head_id
        ));
    }

    second_slot.abort();
    second_slot.await.expect_err("second slot cancelled");
    server.abort();
    server.await.expect_err("server cancelled");
    drop(connection);
    drop(client_endpoint);
}

#[test]
fn coalesced_unfollow_readd_cancels_active_epoch_and_prunes_stale_state() {
    let followee_id = RostraIdSecretKey::generate().id();
    let stale_head = build_event(RostraIdSecretKey::generate(), 9, None)
        .event
        .event_id
        .to_short();
    let old_epoch = build_event(RostraIdSecretKey::generate(), 10, None)
        .event
        .event_id
        .to_short();
    let new_epoch = build_event(RostraIdSecretKey::generate(), 11, None)
        .event
        .event_id
        .to_short();
    let mut desired = HashMap::from([(followee_id, old_epoch)]);
    let mut pending = BTreeMap::new();
    let (cancel, mut cancelled) = oneshot::channel();
    let mut active = HashMap::from([(
        followee_id,
        ActiveFolloweePoll {
            epoch: old_epoch,
            cancel,
        },
    )]);
    let mut states = HashMap::from([(
        followee_id,
        Arc::new(RwLock::new(FolloweePollState {
            remote_progress: RemoteProgress::Pending(stale_head),
            ..FolloweePollState::default()
        })),
    )]);

    PollFolloweeHeadUpdates::reconcile_followee_epochs(
        HashMap::from([(followee_id, new_epoch)]),
        &mut desired,
        &mut pending,
        &mut active,
        &mut states,
    );
    assert_eq!(cancelled.try_recv(), Ok(()));
    assert!(active.is_empty());
    assert_eq!(pending.get(&followee_id), Some(&new_epoch));
    assert!(states.is_empty());

    let readded = states.entry(followee_id).or_default().clone();
    assert_eq!(
        readded.blocking_read().wait_cursor(ShortEventId::ZERO),
        ShortEventId::ZERO
    );
    assert_eq!(states.len(), 1);

    PollFolloweeHeadUpdates::reconcile_followee_epochs(
        HashMap::new(),
        &mut desired,
        &mut pending,
        &mut active,
        &mut states,
    );
    assert!(states.is_empty());
    assert!(pending.is_empty());
}
