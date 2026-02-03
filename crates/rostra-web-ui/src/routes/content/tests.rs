use std::str::FromStr;

use jotup::r#async::AsyncRenderOutputExt;
use rostra_core::id::RostraId;

use super::RostraRenderExt;
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

/// Valid base32 test event ID (16 bytes = 26 base32 characters)
const TEST_EVENT_ID: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn extract_rostra_media_link() {
    assert_eq!(
        UiState::extra_rostra_media_link(&format!("rostra-media:{TEST_EVENT_ID}")),
        Some(rostra_core::ShortEventId::from_str(TEST_EVENT_ID).unwrap())
    );
    assert_eq!(UiState::extra_rostra_media_link("not-a-media-link"), None);
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

    // Should have language class on code element
    assert!(
        html.contains("language-rust"),
        "Missing language-rust class"
    );
}

#[tokio::test]
async fn code_block_unknown_language() {
    let content = "```\nplain code\n```";

    let html = render_with_prism(content).await;

    // Should still render as code block
    assert!(html.contains("<code"), "Missing code element");
}

#[tokio::test]
async fn inline_code_not_affected_by_prism() {
    let content = "Some `inline code` here";

    let html = render_with_prism(content).await;

    // Inline code should not get language class
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

    // The apostrophe in "I'ts" is parsed as a RightSingleQuote event between Str
    // events
    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightSingleQuote)),
        "Expected RightSingleQuote event for the apostrophe"
    );

    // Check the Str events contain "I" and "ts" separately
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
    // Test various smart punctuation in alt text
    let content = r#"![It's "great"...](https://example.com/img.png)"#;
    let events = render_events(content);

    // Should have right single quote, double quotes, and ellipsis
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
    // Test that multi-line alt text generates Softbreak events
    let content = "![line1\nline2](https://example.com/img.png)";
    let events = render_events(content);

    assert!(
        events.iter().any(|e| matches!(e, jotup::Event::Softbreak)),
        "Expected Softbreak event for newline in alt text"
    );

    // Test symbol syntax in alt text
    let content_sym = "![a :smile: emoji](https://example.com/img.png)";
    let events_sym = render_events(content_sym);

    assert!(
        events_sym
            .iter()
            .any(|e| matches!(e, jotup::Event::Symbol(_))),
        "Expected Symbol event for :smile: in alt text"
    );
}
