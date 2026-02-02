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
