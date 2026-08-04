//! Link extraction utilities for Rostra-specific djot links.

use std::str::FromStr;

use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ShortRostraId};

/// An identity reference accepted in a `rostra:` link.
///
/// Short references identify a collision-protected prefix and require a
/// caller-owned identity index before they can become full identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RostraIdLink {
    /// A complete Rostra identity.
    Full(RostraId),
    /// A canonical shortened Rostra identity prefix.
    Short(ShortRostraId),
}

/// Extract a RostraId from a `rostra:` link.
///
/// Returns `Some(RostraId)` if the string starts with `rostra:` and the
/// remainder is a valid RostraId.
pub fn extract_rostra_id_link(s: &str) -> Option<RostraId> {
    s.strip_prefix("rostra:")
        .and_then(|s| RostraId::from_str(s).ok())
}

/// Extract a full or canonical short Rostra identity reference from a `rostra:`
/// link.
///
/// This preserves [`extract_rostra_id_link`] for callers that require a full
/// identity while allowing callers with an identity index to resolve short
/// references explicitly.
pub fn extract_rostra_id_link_reference(s: &str) -> Option<RostraIdLink> {
    let id = s.strip_prefix("rostra:")?;

    RostraId::from_str(id)
        .map(RostraIdLink::Full)
        .or_else(|_| ShortRostraId::from_str(id).map(RostraIdLink::Short))
        .ok()
}

/// Extract a ShortEventId from a `rostra-media:` link.
///
/// Returns `Some(ShortEventId)` if the string starts with `rostra-media:` and
/// the remainder is a valid ShortEventId.
pub fn extract_rostra_media_link(s: &str) -> Option<ShortEventId> {
    s.strip_prefix("rostra-media:")
        .and_then(|s| ShortEventId::from_str(s).ok())
}
