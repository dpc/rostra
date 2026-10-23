//! Mention detection in djot content.

use jotup::{Container, Event};
use rostra_core::id::{RostraId, ToShort as _};

use crate::links::{RostraIdLink, extract_rostra_id_link_reference};

/// Check if djot content contains a mention of the target RostraId.
///
/// This function parses the djot content and looks for `rostra:<id>` links
/// where the full identifier or canonical short prefix matches the target.
///
/// Before parsing, it applies the same source-size and conservative recursive
/// block-opener guard as [`crate::native::render`]. Over-budget content returns
/// false without parsing: ingestion retains the signed post and original bytes,
/// but may omit its mention index/notification. This includes false negatives
/// for more than 64 flat list items or apparent openers in literal code.
/// Ordinary in-budget mention matching is unchanged.
pub fn contains_mention(djot_content: &str, target_id: RostraId) -> bool {
    if crate::parse_guard::check(djot_content).is_err() {
        return false;
    }
    for event in jotup::Parser::new(djot_content) {
        if let Event::Start(Container::Link(url, _), _) = event {
            let is_target = match extract_rostra_id_link_reference(&url) {
                Some(RostraIdLink::Full(mentioned_id)) => mentioned_id == target_id,
                Some(RostraIdLink::Short(mentioned_id)) => mentioned_id == target_id.to_short(),
                None => false,
            };
            if is_target {
                return true;
            }
        }
    }
    false
}
