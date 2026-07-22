use std::collections::HashSet;
use std::time::Duration;

use iroh::endpoint::presets;
use rostra_client_db::Database;
use rostra_core::ShortEventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;

use super::HeadMerger;
use crate::Client;

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn merges_preexisting_durable_forks_on_startup() {
    let id_secret = RostraIdSecretKey::generate();
    let id = id_secret.id();
    let db = Database::new_in_memory(id)
        .await
        .expect("in-memory database");

    let mut original_heads = HashSet::new();
    for bytes in [[1], [2], [3]] {
        let content = EventContentRaw::new(bytes.to_vec());
        let signed = Event::builder_raw_content()
            .author(id)
            .kind(EventKind::NULL)
            .content(&content)
            .build()
            .signed_by(id_secret);
        let event = rostra_core::event::VerifiedEvent::verify_signed(id, signed)
            .expect("self-signed event");
        original_heads.insert(event.event_id.to_short());
        let event = VerifiedEventContent::verify(event, content).expect("matching content");
        db.process_event_with_content(&event).await;
    }
    assert_eq!(db.get_heads_self().await, original_heads);

    let endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![ROSTRA_P2P_V0_ALPN.to_vec()])
        .bind()
        .await
        .expect("test endpoint");
    let client = Client::builder(id)
        .db(db)
        .iroh_endpoint(endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("test client");
    let db = client.db().clone();

    let mut merger = HeadMerger::new(&client, id_secret);
    merger.max_merge_delay = Duration::ZERO;
    let merger = tokio::spawn(merger.run());

    let merged_head = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let heads = db.get_heads_self().await;
            if heads.len() == 1 {
                break *heads.iter().next().expect("singleton checked");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pre-existing forks must be merged without another announcement");
    merger.abort();

    let mut reachable_originals = HashSet::new();
    let mut pending = vec![merged_head];
    let mut visited = HashSet::new();
    while let Some(event_id) = pending.pop() {
        if !visited.insert(event_id) {
            continue;
        }
        if original_heads.contains(&event_id) {
            reachable_originals.insert(event_id);
        }

        let event = db.get_event(event_id).await.expect("ancestor event stored");
        for parent in [
            Option::<ShortEventId>::from(event.signed.event.parent_prev),
            Option::<ShortEventId>::from(event.signed.event.parent_aux),
        ]
        .into_iter()
        .flatten()
        {
            pending.push(parent);
        }
    }
    assert_eq!(reachable_originals, original_heads);
}
