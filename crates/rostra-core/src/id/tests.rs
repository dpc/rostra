use std::str::FromStr as _;

use super::{RostraId, ShortRostraId, ToShort as _};

#[test]
fn short_rostra_id_parses_its_canonical_encoding() {
    let short_id = RostraId::from_bytes([42; 32]).to_short();

    assert_eq!(
        short_id
            .to_string()
            .parse::<ShortRostraId>()
            .expect("canonical short ID parses"),
        short_id
    );
}

#[test]
fn short_rostra_id_rejects_invalid_encodings() {
    for input in [
        "not-a-rostra-id".to_owned(),
        "rsnot-valid".to_owned(),
        format!("rs{}", z32::encode(&[42; 15])),
        format!("rs{}", z32::encode(&[42; 17])),
    ] {
        assert!(ShortRostraId::from_str(&input).is_err(), "{input}");
    }
}
