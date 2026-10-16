use rostra_core::event::SocialPost;
use rostra_core::id::RostraId;
use rostra_core::{ExternalEventId, ShortEventId};
use rostra_djot::extract::SocialExcerpt;

use super::{DESCRIPTION_MAX_CHARS, display_name_or_short_id};

fn social_metadata(
    excerpt: &SocialExcerpt,
    author: &str,
    is_reply: bool,
    reply_target: Option<&str>,
) -> super::SocialMetadata {
    let reply_to =
        is_reply.then(|| ExternalEventId::new(RostraId::from_bytes([42; 32]), ShortEventId::ZERO));
    let post = SocialPost::new_text(String::new(), reply_to, Default::default());
    super::social_metadata(excerpt, author, &post, reply_target)
}

fn excerpt(heading: Option<&str>, paragraphs: &[&str]) -> SocialExcerpt {
    SocialExcerpt {
        first_heading: heading.map(str::to_owned),
        paragraphs: paragraphs.iter().map(ToString::to_string).collect(),
        ..Default::default()
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
fn heading_only_content_has_a_useful_description() {
    let metadata = social_metadata(&excerpt(Some("Title"), &[]), "Alice", false, None);

    assert_eq!(metadata.title, "Title");
    assert_eq!(metadata.description, "Title");
}

#[test]
fn link_post_description_uses_the_visible_title_even_without_a_body() {
    let post = SocialPost::new_text(String::new(), None, Default::default()).with_news_fields(
        Some(
            "https://play.rust-lang.org/?version=stable&mode=debug&edition=2024"
                .parse()
                .unwrap(),
        ),
        Some("Rust playground can now handle multiple files".to_owned()),
    );
    let metadata = super::social_metadata(&excerpt(None, &[]), "dpc", &post, None);
    assert_eq!(
        metadata.description,
        "Rust playground can now handle multiple files"
    );
}

#[test]
fn link_title_and_body_are_normalized_readable_and_bounded() {
    let mut post = SocialPost::new_text(String::new(), None, Default::default());
    post.title = Some("  A\tuseful\nlink  ".to_owned());
    let metadata =
        super::social_metadata(&excerpt(None, &["My commentary."]), "Alice", &post, None);
    assert_eq!(metadata.description, "A useful link\n\nMy commentary.");

    post.title = Some("界".repeat(DESCRIPTION_MAX_CHARS + 1));
    let metadata = super::social_metadata(&excerpt(None, &[]), "Alice", &post, None);
    assert_eq!(metadata.description.chars().count(), DESCRIPTION_MAX_CHARS);
    assert!(metadata.description.ends_with('…'));
}

#[test]
fn untitled_link_media_and_empty_posts_have_fallbacks() {
    let mut post = SocialPost::new_text(String::new(), None, Default::default());
    post.title = Some(" \n ".to_owned());
    post.url = Some("https://example.com/story".parse().unwrap());
    assert_eq!(
        super::social_metadata(&excerpt(None, &[]), "Alice", &post, None).description,
        "https://example.com/story"
    );
    post.url = None;
    let media = rostra_djot::extract::extract_social_excerpt(
        "![](rostra-media:AAAAAAAAAAAAAAAAAAAAAAAAAA)",
    );
    assert_eq!(
        super::social_metadata(&media, "Alice", &post, None).description,
        "Media attachment"
    );
    let captioned = rostra_djot::extract::extract_social_excerpt(
        "![A mountain at sunset](rostra-media:AAAAAAAAAAAAAAAAAAAAAAAAAA)",
    );
    assert_eq!(
        super::social_metadata(&captioned, "Alice", &post, None).description,
        "A mountain at sunset"
    );
    assert_eq!(
        super::social_metadata(&excerpt(None, &[]), "Alice", &post, None).description,
        "Alice's post on Rostra"
    );
}

#[test]
fn social_description_attributes_preserve_text_without_creating_markup() {
    use crate::layout::{OpenGraphMeta, PageResources};

    let mut post = SocialPost::new_text(String::new(), None, Default::default());
    post.title = Some("世界 \"quoted\" & <script>alert(1)</script>".to_owned());
    let metadata = super::social_metadata(&excerpt(None, &[]), "Alice", &post, None);
    let og = OpenGraphMeta {
        title: metadata.title,
        description: metadata.description.clone(),
        url: "https://example.com/post".to_owned(),
        image: None,
    };
    let markup = crate::UiState::render_html_head(
        &og.title,
        None,
        Some(&og),
        None,
        false,
        PageResources::Standard,
    )
    .into_string();
    let html = scraper::Html::parse_fragment(&markup);
    for selector in [
        "meta[name='description']",
        "meta[property='og:description']",
        "meta[name='twitter:description']",
    ] {
        let selector = scraper::Selector::parse(selector).unwrap();
        let element = html.select(&selector).next().unwrap();
        assert_eq!(
            element.value().attr("content"),
            Some(metadata.description.as_str())
        );
    }
    assert!(!markup.contains("<script>alert(1)</script>"));
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
