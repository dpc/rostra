//! Synthetic local routing regression, not a relay/mobile acceptance
//! substitute.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::Database;
use rostra_core::ShortEventId;
use rostra_core::event::content_kind::{Follow, NodeAnnouncement, PersonaSelector};
use rostra_core::event::{
    Event, EventContentKind as _, EventContentRaw, EventKind, IrohNodeId, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;

use super::{BroadcastHeadOutcome, HeadUpdateBroadcaster};
use crate::{Client, PkarrClient};

fn signed(
    secret: RostraIdSecretKey,
    kind: EventKind,
    raw: EventContentRaw,
    prev: Option<ShortEventId>,
    aux: Option<ShortEventId>,
) -> VerifiedEventContent {
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(kind)
        .maybe_parent_prev(prev)
        .maybe_parent_aux(aux)
        .content(&raw)
        .build()
        .signed_by(secret);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), signed).unwrap(),
        raw,
    )
    .unwrap()
}

fn node(secret: RostraIdSecretKey, endpoint: &iroh::Endpoint) -> VerifiedEventContent {
    signed(
        secret,
        EventKind::NODE_ANNOUNCEMENT,
        NodeAnnouncement::Iroh {
            addr: IrohNodeId::from_bytes(*endpoint.id().as_bytes()),
        }
        .serialize_cbor()
        .unwrap(),
        None,
        None,
    )
}

fn device(secret: RostraIdSecretKey, id: u8, parent: ShortEventId) -> VerifiedEventContent {
    let epoch = rostra_dm::LocalEpoch::generate(rostra_core::Timestamp::now().as_u64()).unwrap();
    signed(
        secret,
        EventKind::DM_DEVICE,
        EventContentRaw::new(
            rostra_dm::Announcement {
                device_id: [id; 16],
                epoch: Some(epoch.public().clone()),
            }
            .encode()
            .unwrap(),
        ),
        Some(parent),
        None,
    )
}

async fn endpoint(lookup: &iroh::address_lookup::memory::MemoryLookup) -> iroh::Endpoint {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

async fn client(
    secret: RostraIdSecretKey,
    endpoint: iroh::Endpoint,
    pkarr: Arc<PkarrClient>,
) -> Arc<Client> {
    Client::builder(secret.id())
        .db(Database::new_in_memory(secret.id()).await.unwrap())
        .iroh_endpoint(endpoint)
        .pkarr_client(pkarr)
        .start_background_tasks(false)
        .build()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn current_head_feed_routes_same_identity_ancestors_without_replacing_account_route() {
    tokio::time::timeout(Duration::from_secs(10), async {
        // No DHT, public relay, DNS discovery, or real identity participates.
        // Unexpected Pkarr fallback can only hit this local unavailable backend.
        let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut builder = pkarr::Client::builder();
        builder
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
            Arc::new(PkarrClient::build(builder, NonZeroUsize::new(32).unwrap(), 30, 30).unwrap());
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();
        let shared = RostraIdSecretKey::generate();
        let observer = RostraIdSecretKey::generate();
        let a_endpoint = endpoint(&lookup).await;
        let b_endpoint = endpoint(&lookup).await;
        let s_endpoint = endpoint(&lookup).await;
        let a_node = node(shared, &a_endpoint);
        let b_node = node(shared, &b_endpoint);
        let s_node = node(observer, &s_endpoint);
        let a_device = device(shared, 1, a_node.event_id().to_short());
        let b_device = device(shared, 2, b_node.event_id().to_short());
        let s_follow = signed(
            observer,
            EventKind::FOLLOW,
            Follow {
                followee: shared.id(),
                persona: None,
                selector: Some(PersonaSelector::Except { ids: vec![] }),
                persona_tags_selector: None,
            }
            .serialize_cbor()
            .unwrap(),
            Some(s_node.event_id().to_short()),
            None,
        );
        let a = client(shared, a_endpoint.clone(), pkarr.clone()).await;
        let b = client(shared, b_endpoint, pkarr.clone()).await;
        let s = client(observer, s_endpoint, pkarr).await;

        // Synthetic initial topology: A has learned B; B and S know only B's
        // endpoint and branch. All three retain the authentic follow relation.
        for fixture in [&a, &b, &s] {
            for event in [&b_node, &b_device, &s_node, &s_follow] {
                fixture
                    .db()
                    .try_process_event_with_content(event)
                    .await
                    .unwrap();
            }
        }
        for event in [&a_node, &a_device] {
            a.db().try_process_event_with_content(event).await.unwrap();
        }
        assert_eq!(a.db().dm_own_devices(None, 64).await.unwrap().len(), 2);
        assert_eq!(b.db().dm_own_devices(None, 64).await.unwrap().len(), 1);
        let held = s
            .connection_cache()
            .get_or_connect(s.networking(), shared.id())
            .await
            .unwrap();
        assert_eq!(held.remote_id(), b.networking().endpoint.id());
        let merged = signed(
            shared,
            EventKind::NULL,
            EventContentRaw::new(vec![]),
            Some(a_device.event_id().to_short()),
            Some(b_device.event_id().to_short()),
        );
        a.db()
            .try_process_event_with_content(&merged)
            .await
            .unwrap();
        assert_eq!(
            a.db().get_heads_self().await,
            BTreeSet::from([merged.event_id().to_short()])
                .into_iter()
                .collect()
        );

        // Exercise the actual broadcaster and actual inbound FEED_EVENT handlers,
        // including its "already have" response for an empty merge payload.
        let broadcaster = HeadUpdateBroadcaster::new(&a);
        let event = a.db().get_event(merged.event_id()).await.unwrap();
        let followers = broadcaster.self_followers.snapshot();
        let result = broadcaster
            .broadcast_head(
                merged.event_id().to_short(),
                &event,
                &EventContentRaw::new(vec![]),
                &followers,
                Duration::from_secs(2),
            )
            .await;
        assert!(matches!(result.outcome, BroadcastHeadOutcome::Delivered));
        assert_eq!(result.delivered, 2);
        for receiver in [&b, &s] {
            assert!(receiver.db().get_event(merged.event_id()).await.is_some());
            assert!(receiver.db().get_event(a_device.event_id()).await.is_none());
            assert!(
                receiver
                    .db()
                    .get_missing_events_for_id(shared.id())
                    .await
                    .contains(&a_device.event_id().to_short())
            );
        }

        // Independent positive control, without ingesting or repairing anything.
        let direct = s
            .networking()
            .connect_ticket(a_endpoint.addr().into())
            .await
            .unwrap();
        assert!(
            direct
                .get_event(shared.id(), a_device.event_id().to_short())
                .await
                .unwrap()
                .is_some()
        );
        assert!(s.db().get_event(a_device.event_id()).await.is_none());

        // Ordinary acquisition now uses the observed holder while the account
        // route remains pinned at B. A's signed node announcement is itself an
        // ancestor, so reaching it also exercises inherited holder deadlines.
        let peers = s
            .db()
            .get_followers(shared.id())
            .await
            .into_iter()
            .chain([shared.id(), observer.id()])
            .collect::<Vec<_>>();
        assert!(
            crate::util::rpc::download_events_from_child(
                shared.id(),
                a_device.event_id().to_short(),
                s.networking(),
                s.connection_cache(),
                &peers,
                s.db()
            )
            .await
            .unwrap()
        );
        assert!(s.db().get_event(a_device.event_id()).await.is_some());
        assert!(
            s.db()
                .get_event_content(a_device.event_id())
                .await
                .is_some()
        );
        assert!(s.db().get_event_content(a_node.event_id()).await.is_some());
        let selected = s.db().dm_destinations_now(shared.id()).await.unwrap();
        assert_eq!(selected.keys().len(), 2);
        assert_eq!(
            s.connection_cache()
                .get_or_connect(s.networking(), shared.id())
                .await
                .unwrap()
                .remote_id(),
            b.networking().endpoint.id()
        );
        drop((direct, held, broadcaster));
        for fixture in [a, b, s] {
            let mut shutdown = fixture.begin_shutdown().unwrap();
            shutdown.join().await.unwrap();
        }
    })
    .await
    .expect("bounded local-only bootstrap regression");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn duplicate_head_from_non_author_holder_routes_through_retained_parent() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut builder = pkarr::Client::builder();
        builder
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
            Arc::new(PkarrClient::build(builder, NonZeroUsize::new(32).unwrap(), 30, 30).unwrap());
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();
        let author = RostraIdSecretKey::generate();
        let forwarder_id = RostraIdSecretKey::generate();
        let receiver_endpoint = endpoint(&lookup).await;
        let receiver = client(author, receiver_endpoint.clone(), pkarr.clone()).await;
        let forwarder = client(forwarder_id, endpoint(&lookup).await, pkarr).await;
        let grandparent = signed(
            author,
            EventKind::NULL,
            EventContentRaw::new(vec![]),
            None,
            None,
        );
        let parent = signed(
            author,
            EventKind::NULL,
            EventContentRaw::new(vec![]),
            Some(grandparent.event_id().to_short()),
            None,
        );
        let head = signed(
            author,
            EventKind::NULL,
            EventContentRaw::new(vec![]),
            Some(parent.event_id().to_short()),
            None,
        );
        forwarder
            .db()
            .try_process_event_with_content(&grandparent)
            .await
            .unwrap();
        receiver
            .db()
            .try_process_event_with_content(&parent)
            .await
            .unwrap();
        forwarder
            .db()
            .try_process_event_with_content(&parent)
            .await
            .unwrap();
        forwarder
            .db()
            .try_process_event_with_content(&head)
            .await
            .unwrap();
        receiver
            .db()
            .try_process_event_with_content(&head)
            .await
            .unwrap();

        // Start ordinary durable reconciliation before any route is available.
        // It cannot discover the unrelated forwarding account from author lookup.
        let worker = crate::task::missing_event_fetcher::MissingEventFetcher::new(&receiver);
        receiver.task_handles.spawn(worker.run());
        // Force the first ordinary pass to start before the later source feed.
        // The HTTP-only unavailable backend has its own explicit 100ms timeout;
        // production acquisition deadlines remain unchanged.
        let (_stalled_backend, _) = tokio::time::timeout(Duration::from_secs(2), relay.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(receiver.db().get_event(parent.event_id()).await.is_some());
        assert!(
            receiver
                .db()
                .get_event(grandparent.event_id())
                .await
                .is_none()
        );
        let connection = forwarder
            .networking()
            .connect_ticket(receiver_endpoint.addr().into())
            .await
            .unwrap();
        let unwanted = signed(
            forwarder_id,
            EventKind::NULL,
            EventContentRaw::new(vec![]),
            None,
            None,
        );
        assert!(
            connection
                .feed_event(unwanted.event.into(), EventContentRaw::new(vec![]))
                .await
                .is_err()
        );
        assert!(receiver.db().get_event(unwanted.event_id()).await.is_none());
        assert!(
            receiver
                .connection_cache()
                .holders
                .take(forwarder_id.id(), unwanted.event_id().to_short())
                .is_none()
        );
        let mut invalid: rostra_core::event::SignedEvent = head.event.into();
        invalid.sig = rostra_core::event::EventSignature::from_bytes([0; 64]);
        assert!(
            connection
                .feed_event(invalid, EventContentRaw::new(vec![]))
                .await
                .is_err()
        );
        assert!(
            receiver
                .connection_cache()
                .holders
                .take(author.id(), parent.event_id().to_short())
                .is_none()
        );
        // Empty duplicate heads return "already have", but still convey a useful
        // authenticated source; RPC status is not an acceptance bypass.
        let _ = connection
            .feed_event(head.event.into(), EventContentRaw::new(vec![]))
            .await;
        while receiver
            .db()
            .get_event(grandparent.event_id())
            .await
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            receiver
                .db()
                .get_missing_events_for_id(author.id())
                .await
                .is_empty()
        );
        assert!(receiver.db().get_id_endpoints(author.id()).await.is_empty());
        assert!(
            receiver
                .db()
                .get_id_endpoints(forwarder_id.id())
                .await
                .is_empty()
        );
        drop(connection);
        for fixture in [receiver, forwarder] {
            let mut shutdown = fixture.begin_shutdown().unwrap();
            shutdown.join().await.unwrap();
        }
    })
    .await
    .expect("bounded duplicate-head forwarding regression");
}
