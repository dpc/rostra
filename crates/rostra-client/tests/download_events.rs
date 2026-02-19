use rostra_client::Client;
use rostra_client_db::Database;
use rostra_core::event::content_kind::IrohNodeId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;

fn build_test_event(
    id_secret: RostraIdSecretKey,
    parent_prev: impl Into<Option<ShortEventId>>,
) -> (VerifiedEvent, VerifiedEventContent) {
    let parent = parent_prev.into();
    let content_bytes = b"test content";
    let content = EventContentRaw::new(content_bytes.to_vec());
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent)
        .content(&content)
        .build();

    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
    let verified_content =
        VerifiedEventContent::verify(verified_event, content).expect("Valid content");
    (verified_event, verified_content)
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
    let ep_a = iroh::Endpoint::empty_builder(iroh::RelayMode::Disabled)
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
    let ep_b = iroh::Endpoint::empty_builder(iroh::RelayMode::Disabled)
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

    // Client B calls download_events_from_child to fetch all events
    let connections = client_b.connection_cache().clone();
    let peers = vec![id_a, id_b];

    let downloaded = rostra_client::util::rpc::download_events_from_child(
        id_a,
        head_id,
        client_b.networking(),
        &connections,
        &peers,
        db_b,
    )
    .await
    .expect("download_events_from_child should not fail");

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

    Ok(())
}
