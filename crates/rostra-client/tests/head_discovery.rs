use std::time::Duration;

use rostra_client::Client;
use rostra_client_db::Database;
use rostra_core::ShortEventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p::Connection;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;

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
async fn wait_head_update_does_not_expose_existing_sibling() {
    let id_secret = RostraIdSecretKey::generate();
    let id = id_secret.id();
    let db = Database::new_in_memory(id)
        .await
        .expect("in-memory database");

    let first = build_event(id_secret, 1, None);
    let first_id = first.event.event_id.to_short();
    db.process_event_with_content(&first).await;

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

    let server = Client::builder(id)
        .db(db)
        .iroh_endpoint(server_endpoint)
        .start_background_tasks(false)
        .build()
        .await
        .expect("server client");
    let db = server.db().clone();

    let caller_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .address_lookup(lookup)
        .bind()
        .await
        .expect("caller endpoint");
    let connection = Connection::from(
        caller_endpoint
            .connect(server_id, ROSTRA_P2P_V0_ALPN)
            .await
            .expect("connect to server"),
    );

    let mut wait = Box::pin(connection.wait_head_update(first_id));
    tokio::task::yield_now().await;

    let sibling = build_event(id_secret, 2, None);
    let sibling_id = sibling.event.event_id.to_short();
    db.process_event_with_content(&sibling).await;
    assert_eq!(db.get_heads_self().await.len(), 2);

    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut wait)
            .await
            .is_err(),
        "an existing sibling must not complete the legacy one-head cursor"
    );

    let replacement = build_event(id_secret, 3, Some(first_id));
    let replacement_id = replacement.event.event_id.to_short();
    db.process_event_with_content(&replacement).await;

    let returned = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("removing the known head must wake the waiter")
        .expect("WAIT_HEAD_UPDATE response");
    assert!(
        [sibling_id, replacement_id].contains(&returned),
        "response must sample the current head set"
    );
}
