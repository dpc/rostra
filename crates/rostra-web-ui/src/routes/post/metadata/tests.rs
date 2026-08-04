use rostra_djot::extract::SocialExcerpt;

use super::{DESCRIPTION_MAX_CHARS, display_name_or_short_id, social_metadata};

fn excerpt(heading: Option<&str>, paragraphs: &[&str]) -> SocialExcerpt {
    SocialExcerpt {
        first_heading: heading.map(str::to_owned),
        paragraphs: paragraphs.iter().map(ToString::to_string).collect(),
    }
}

#[test]
fn heading_and_paragraphs_supply_shared_metadata() {
    let metadata = social_metadata(
        &excerpt(Some("Title"), &["First paragraph.", "Second paragraph."]),
        "Alice",
        false,
        None,
    );

    assert_eq!(metadata.title, "Title");
    assert_eq!(
        metadata.description,
        "First paragraph.\n\nSecond paragraph."
    );
}

#[test]
fn paragraph_only_content_uses_the_authored_title() {
    let metadata = social_metadata(
        &excerpt(None, &["A paragraph without a heading."]),
        "Alice",
        false,
        None,
    );

    assert_eq!(metadata.title, "Alice's post on Rostra");
    assert_eq!(metadata.description, "A paragraph without a heading.");
}

#[test]
fn authored_fallbacks_describe_posts_and_replies() {
    assert_eq!(
        social_metadata(&excerpt(None, &[]), "Alice", false, None).title,
        "Alice's post on Rostra"
    );
    assert_eq!(
        social_metadata(&excerpt(None, &[]), "James", true, Some("Chris")).title,
        "James' reply to Chris' post"
    );
    assert_eq!(
        social_metadata(&excerpt(None, &[]), "James", true, None).title,
        "James' reply to a post"
    );
}

#[test]
fn empty_author_name_falls_back_to_short_id() {
    assert_eq!(display_name_or_short_id(None, "rsshort"), "rsshort");
    assert_eq!(display_name_or_short_id(Some(" \n"), "rsshort"), "rsshort");
    assert_eq!(
        display_name_or_short_id(Some(" Alice "), "rsshort"),
        "Alice"
    );
}

#[test]
fn blank_reply_target_name_uses_its_short_id() {
    let target_name = display_name_or_short_id(Some("  "), "rstarget");
    let metadata = social_metadata(&excerpt(None, &[]), "Alice", true, Some(&target_name));

    assert_eq!(metadata.title, "Alice's reply to rstarget's post");
}

#[test]
fn padded_reply_target_name_is_trimmed() {
    let target_name = display_name_or_short_id(Some("  Chris  "), "rstarget");
    let metadata = social_metadata(&excerpt(None, &[]), "Alice", true, Some(&target_name));

    assert_eq!(metadata.title, "Alice's reply to Chris' post");
}

#[test]
fn heading_only_content_does_not_duplicate_its_title_as_description() {
    let metadata = social_metadata(&excerpt(Some("Title"), &[]), "Alice", false, None);

    assert_eq!(metadata.title, "Title");
    assert!(metadata.description.is_empty());
}

#[test]
fn description_truncates_at_unicode_whitespace() {
    let prefix = "é ".repeat(300);
    let metadata = social_metadata(
        &excerpt(None, &[&format!("{prefix}tail")]),
        "Alice",
        false,
        None,
    );

    assert!(metadata.description.chars().count() <= DESCRIPTION_MAX_CHARS);
    assert!(metadata.description.ends_with('\u{2026}'));
    assert!(!metadata.description.ends_with("tail\u{2026}"));
}

#[test]
fn long_unbroken_description_is_truncated_safely() {
    let metadata = social_metadata(
        &excerpt(None, &[&"界".repeat(DESCRIPTION_MAX_CHARS + 1)]),
        "Alice",
        false,
        None,
    );

    assert_eq!(metadata.description.chars().count(), DESCRIPTION_MAX_CHARS);
    assert!(metadata.description.ends_with('\u{2026}'));
}
