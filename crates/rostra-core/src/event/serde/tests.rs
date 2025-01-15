use tracing::info;

use crate::event::{Event, EventContent, EventKind, SignedEvent};
use crate::id::RostraIdSecretKey;

#[test_log::test]
fn event_size() {
    let id_secret = RostraIdSecretKey::generate();

    let event = Event::builder()
        .author(id_secret.id())
        .kind(EventKind::RAW)
        .content(EventContent::from(b"test".to_vec()))
        .build();

    let event_signed = event.signed_by(id_secret);

    let event_signed_serialized = serde_json::to_string(&event_signed).expect("Can't fail");

    info!(%event_signed_serialized, "event_signed_serialized");

    let event_signed_deserialized: SignedEvent =
        serde_json::from_str(&event_signed_serialized).expect("Can't fail");

    assert_eq!(event_signed, event_signed_deserialized);
}
