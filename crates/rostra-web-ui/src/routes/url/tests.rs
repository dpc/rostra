use std::str::FromStr as _;

use rostra_core::id::ToShort as _;

use super::{EventPathId, RostraPathId};

#[test]
fn path_ids_distinguish_canonical_short_and_legacy_full_forms() {
    let rostra_id = rostra_core::id::RostraId::from_bytes([42; 32]);
    let event_id = rostra_core::EventId::from_bytes([43; 32]);

    for full in [
        rostra_id.to_string(),
        rostra_id.to_bech32_string(),
        rostra_id.to_unprefixed_z32_string(),
    ] {
        assert_eq!(
            RostraPathId::from_str(&full),
            Ok(RostraPathId::Full(rostra_id))
        );
    }
    assert_eq!(
        RostraPathId::from_str(&rostra_id.to_short().to_string()),
        Ok(RostraPathId::Short(rostra_id.to_short()))
    );
    assert_eq!(
        EventPathId::from_str(&event_id.to_string()),
        Ok(EventPathId::Full(event_id))
    );
    assert_eq!(
        EventPathId::from_str(&event_id.to_short().to_string()),
        Ok(EventPathId::Short(event_id.to_short()))
    );
}
