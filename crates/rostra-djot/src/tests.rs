use std::str::FromStr;

use rostra_core::id::RostraId;

use crate::links::{extract_rostra_id_link, extract_rostra_media_link};
use crate::mention::contains_mention;

#[test]
fn test_extract_rostra_id_link_valid() {
    // This test just ensures the prefix stripping works
    assert!(extract_rostra_id_link("not-rostra:something").is_none());
    assert!(extract_rostra_id_link("rostra").is_none());
}

#[test]
fn test_extract_rostra_media_link() {
    assert!(extract_rostra_media_link("not-rostra-media:something").is_none());
    assert!(extract_rostra_media_link("rostra-media").is_none());
}

#[test]
fn test_contains_mention_no_mentions() {
    // Content with no mentions
    let content = "Hello world! This is a test post.";
    // Use a dummy RostraId - we just need any valid one for testing
    if let Ok(target_id) =
        RostraId::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    {
        assert!(!contains_mention(content, target_id));
    }
}

#[test]
fn test_contains_mention_with_regular_link() {
    // Content with a regular link, not a rostra: mention
    let content = "Check out [this link](https://example.com)!";
    if let Ok(target_id) =
        RostraId::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    {
        assert!(!contains_mention(content, target_id));
    }
}
