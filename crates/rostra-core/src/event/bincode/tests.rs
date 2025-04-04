use crate::bincode::STD_BINCODE_CONFIG;
use crate::event::{Event, EventContentRaw, EventKind};
use crate::id::RostraIdSecretKey;

#[test_log::test]
fn event_size() {
    let id_secret = RostraIdSecretKey::generate();
    // let author = id_secret.id();

    let event = Event::builder_raw_content()
        .author(id_secret.id())
        .kind(EventKind::RAW)
        .content(&EventContentRaw::new(vec![1, 2, 3]))
        .build();

    let event_encoded = bincode::encode_to_vec(event, STD_BINCODE_CONFIG).expect("Can't fail");

    assert_eq!(
        event_encoded.len(),
        128,
        "{}",
        data_encoding::HEXLOWER.encode(&event_encoded)
    );

    let signed = event.signed_by(id_secret);

    let signed_encoded = bincode::encode_to_vec(signed, STD_BINCODE_CONFIG).expect("Can't fail");

    assert_eq!(signed_encoded.len(), 192);
}
