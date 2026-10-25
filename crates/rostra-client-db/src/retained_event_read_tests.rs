use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use crate::event::EventRecord;
use crate::{Database, events};

#[tokio::test(flavor = "multi_thread")]
async fn bounded_retained_reader_rejects_corruption_without_turning_it_into_absence() {
    let secret = RostraIdSecretKey::generate();
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&EventContentRaw::new(vec![]))
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_received_as_is(signed).unwrap();
    let db = Database::new_in_memory(secret.id()).await.unwrap();
    assert!(db.try_get_event(event.event_id).await.unwrap().is_none());
    db.try_process_event(&event).await.unwrap();
    assert_eq!(
        db.try_get_event(event.event_id)
            .await
            .unwrap()
            .unwrap()
            .signed,
        signed
    );
    let key =
        bincode::encode_to_vec(event.event_id.to_short(), redb_bincode::BINCODE_CONFIG).unwrap();
    let valid =
        bincode::encode_to_vec(EventRecord { signed }, redb_bincode::BINCODE_CONFIG).unwrap();
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut wrong = signed;
    wrong.event.author = RostraIdSecretKey::generate().id();
    let wrong = bincode::encode_to_vec(EventRecord { signed: wrong }, redb_bincode::BINCODE_CONFIG)
        .unwrap();
    for bytes in [vec![], vec![0; 1025], trailing, wrong] {
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(events::TABLE.as_raw())?
                .insert(key.as_slice(), bytes.as_slice())?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(db.try_get_event(event.event_id).await.is_err());
    }
    // Noncanonical keys cannot alias the canonical requested short key.
    db.write_with(|tx| {
        let mut table = tx.as_raw().open_table(events::TABLE.as_raw())?;
        table.remove(key.as_slice())?;
        let mut noncanonical = key.clone();
        noncanonical.push(0);
        table.insert(noncanonical.as_slice(), valid.as_slice())?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(db.try_get_event(event.event_id).await.unwrap().is_none());
}
