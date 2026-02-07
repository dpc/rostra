//! Mention detection in djot content.

use jotup::{Container, Event};
use rostra_core::id::RostraId;

use crate::links::extract_rostra_id_link;

/// Check if djot content contains a mention of the target RostraId.
///
/// This function parses the djot content and looks for `rostra:<id>` links
/// where the id matches the target. Returns `true` if such a mention is found.
pub fn contains_mention(djot_content: &str, target_id: RostraId) -> bool {
    for event in jotup::Parser::new(djot_content) {
        if let Event::Start(Container::Link(url, _), _) = event {
            if let Some(mentioned_id) = extract_rostra_id_link(&url) {
                if mentioned_id == target_id {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_contains_mention_no_mentions() {
        // Content with no mentions
        let content = "Hello world! This is a test post.";
        // Use a dummy RostraId - we just need any valid one for testing
        if let Ok(target_id) = RostraId::from_str(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ) {
            assert!(!contains_mention(content, target_id));
        }
    }

    #[test]
    fn test_contains_mention_with_regular_link() {
        // Content with a regular link, not a rostra: mention
        let content = "Check out [this link](https://example.com)!";
        if let Ok(target_id) = RostraId::from_str(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ) {
            assert!(!contains_mention(content, target_id));
        }
    }
}
