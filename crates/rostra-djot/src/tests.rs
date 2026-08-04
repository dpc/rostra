use std::str::FromStr;

use rostra_core::id::{RostraId, ToShort as _};

use crate::links::{
    RostraIdLink, extract_rostra_id_link, extract_rostra_id_link_reference,
    extract_rostra_media_link,
};
use crate::mention::contains_mention;

#[test]
fn extracts_full_rostra_id_link_encodings() {
    let id = RostraId::from_bytes([42; 32]);

    for encoding in [
        id.to_string(),
        id.to_bech32_string(),
        id.to_unprefixed_z32_string(),
    ] {
        let link = format!("rostra:{encoding}");
        assert_eq!(extract_rostra_id_link(&link), Some(id));
        assert_eq!(
            extract_rostra_id_link_reference(&link),
            Some(RostraIdLink::Full(id))
        );
    }
}

#[test]
fn extracts_short_rostra_id_link_without_promoting_it_to_full() {
    let id = RostraId::from_bytes([42; 32]);
    let short_id = id.to_short();
    let link = format!("rostra:{short_id}");

    assert_eq!(extract_rostra_id_link(&link), None);
    assert_eq!(
        extract_rostra_id_link_reference(&link),
        Some(RostraIdLink::Short(short_id))
    );
}

#[test]
fn rejects_invalid_rostra_id_links() {
    assert!(extract_rostra_id_link("not-rostra:something").is_none());
    assert!(extract_rostra_id_link("rostra").is_none());
    assert!(extract_rostra_id_link_reference("not-rostra:something").is_none());
    assert!(extract_rostra_id_link_reference("rostra:rsnot-valid").is_none());
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

#[test]
fn contains_mention_matches_full_and_short_rostra_ids() {
    let target_id = RostraId::from_bytes([42; 32]);
    let other_id = RostraId::from_bytes([43; 32]);

    for content in [
        format!("Hello <rostra:{target_id}>"),
        format!("Hello <rostra:{}>", target_id.to_short()),
    ] {
        assert!(contains_mention(&content, target_id));
        assert!(!contains_mention(&content, other_id));
    }
}
