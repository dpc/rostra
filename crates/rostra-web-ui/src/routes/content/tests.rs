use std::str::FromStr;

use rostra_core::id::RostraId;

use crate::UiState;

#[test]
fn extract_rostra_id_link() {
    assert_eq!(
        UiState::extra_rostra_id_link(
            "rostra:rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy"
        ),
        Some(RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap())
    );
}
