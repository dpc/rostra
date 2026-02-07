//! Link extraction utilities for Rostra-specific djot links.

use std::str::FromStr;

use rostra_core::ShortEventId;
use rostra_core::id::RostraId;

/// Extract a RostraId from a `rostra:` link.
///
/// Returns `Some(RostraId)` if the string starts with `rostra:` and the
/// remainder is a valid RostraId.
pub fn extract_rostra_id_link(s: &str) -> Option<RostraId> {
    s.strip_prefix("rostra:")
        .and_then(|s| RostraId::from_str(s).ok())
}

/// Extract a ShortEventId from a `rostra-media:` link.
///
/// Returns `Some(ShortEventId)` if the string starts with `rostra-media:` and
/// the remainder is a valid ShortEventId.
pub fn extract_rostra_media_link(s: &str) -> Option<ShortEventId> {
    s.strip_prefix("rostra-media:")
        .and_then(|s| ShortEventId::from_str(s).ok())
}
