use std::str::FromStr;

use jotup::r#async::AsyncRenderOutputExt;
use rostra_core::id::RostraId;

use super::{RostraRenderExt, make_base_renderer};
use crate::UiState;

mod url_sanitization;
mod xss_sanitization;

#[test]
fn test_extract_rostra_id_link() {
    assert_eq!(
        UiState::extract_rostra_id_link(
            "rostra:rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy"
        ),
        Some(RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap())
    );
}

/// Valid base32 test event ID (16 bytes = 26 base32 characters)
const TEST_EVENT_ID: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn test_extract_rostra_media_link() {
    assert_eq!(
        UiState::extract_rostra_media_link(&format!("rostra-media:{TEST_EVENT_ID}")),
        Some(rostra_core::ShortEventId::from_str(TEST_EVENT_ID).unwrap())
    );
    assert_eq!(UiState::extract_rostra_media_link("not-a-media-link"), None);
}

/// Helper to render djot content with code block filter only
async fn render_with_prism(content: &str) -> String {
    let renderer = jotup::html::tokio::Renderer::default().prism_code_blocks();

    let out = renderer
        .render_into_document(content)
        .await
        .expect("Rendering failed");

    String::from_utf8(out.into_inner()).expect("valid utf8")
}

// Note: Tests for rostra-media rendering and external image lazy-loading
// require a database client and are tested via integration tests.

#[tokio::test]
async fn code_block_gets_prism_classes() {
    let content = "```rust\nfn main() {}\n```";

    let html = render_with_prism(content).await;

    assert!(
        html.contains("language-rust"),
        "Missing language-rust class"
    );
}

#[tokio::test]
async fn code_block_unknown_language() {
    let content = "```\nplain code\n```";

    let html = render_with_prism(content).await;

    assert!(html.contains("<code"), "Missing code element");
}

#[tokio::test]
async fn inline_code_not_affected_by_prism() {
    let content = "Some `inline code` here";

    let html = render_with_prism(content).await;

    assert!(
        !html.contains("language-"),
        "Inline code should not have language class"
    );
    assert!(
        html.contains("<code>inline code</code>"),
        "Missing inline code"
    );
}

/// Helper to render djot content and see raw djot events
fn render_events(content: &str) -> Vec<jotup::Event<'_>> {
    jotup::Parser::new(content).collect()
}

#[test]
fn djot_image_with_apostrophe_events() {
    // Test that djot parses apostrophes in image alt text as separate events.
    // This is important because our RostraMedia filter must handle smart
    // punctuation events (like RightSingleQuote) inside alt text, not pass them
    // through.
    let content = r#"![I'ts](https://www.youtube.com/watch?v=Z0GFRcFm-aY)"#;
    let events = render_events(content);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightSingleQuote)),
        "Expected RightSingleQuote event for the apostrophe"
    );

    let str_contents: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            jotup::Event::Str(s) => Some(s.as_ref()),
            _ => None,
        })
        .collect();
    assert!(
        str_contents.contains(&"I"),
        "Expected 'I' before apostrophe"
    );
    assert!(
        str_contents.contains(&"ts"),
        "Expected 'ts' after apostrophe"
    );
}

#[test]
fn djot_image_with_multiple_smart_punctuation() {
    let content = r#"![It's "great"...](https://example.com/img.png)"#;
    let events = render_events(content);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightSingleQuote))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::LeftDoubleQuote))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightDoubleQuote))
    );
    assert!(events.iter().any(|e| matches!(e, jotup::Event::Ellipsis)));
}

#[test]
fn djot_image_with_softbreak_and_symbol() {
    let content = "![line1\nline2](https://example.com/img.png)";
    let events = render_events(content);

    assert!(
        events.iter().any(|e| matches!(e, jotup::Event::Softbreak)),
        "Expected Softbreak event for newline in alt text"
    );

    let content_sym = "![a :smile: emoji](https://example.com/img.png)";
    let events_sym = render_events(content_sym);

    assert!(
        events_sym
            .iter()
            .any(|e| matches!(e, jotup::Event::Symbol(_))),
        "Expected Symbol event for :smile: in alt text"
    );
}

/// Helper to render djot content with full sanitization (like production).
/// Uses the same sanitization chain as production code via
/// `make_base_renderer`.
pub(super) async fn render_sanitized(content: &str) -> String {
    let out = make_base_renderer(jotup::html::tokio::Renderer::default())
        .render_into_document(content)
        .await
        .expect("Rendering failed");

    String::from_utf8(out.into_inner()).expect("valid utf8")
}
