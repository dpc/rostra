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

/// Helper to render djot content with image filter only
async fn render_with_images(content: &str, author_id: RostraId) -> String {
    let renderer = jotup::html::tokio::Renderer::default().rostra_images(author_id);

    let out = renderer
        .render_into_document(content)
        .await
        .expect("Rendering failed");

    String::from_utf8(out.into_inner()).expect("valid utf8")
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

#[tokio::test]
async fn rostra_media_renders_with_download_fallback() {
    let author_id =
        RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap();
    let content = format!("![media](rostra-media:{TEST_EVENT_ID})");

    let html = render_with_images(&content, author_id).await;

    // Should contain the wrapper span
    assert!(html.contains("m-rostraMedia"), "Missing wrapper class");

    // Should contain the image with correct URL
    assert!(
        html.contains(&format!(
            "/ui/media/rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy/{TEST_EVENT_ID}"
        )),
        "Missing media URL"
    );

    // Should contain onerror handler for download fallback
    assert!(html.contains("onerror="), "Missing onerror handler");
    assert!(
        html.contains("m-rostraMedia__download"),
        "Missing download class in fallback"
    );
    assert!(
        html.contains("m-rostraMedia__downloadIcon"),
        "Missing download icon class"
    );
}

#[tokio::test]
async fn rostra_media_uses_alt_text_in_fallback() {
    let author_id =
        RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap();
    let content = format!("![my cool file](rostra-media:{TEST_EVENT_ID})");

    let html = render_with_images(&content, author_id).await;

    // Should use alt text in the download link
    assert!(
        html.contains("my cool file"),
        "Missing alt text in fallback"
    );

    // Should sanitize filename (spaces become dashes)
    assert!(
        html.contains("my-cool-file"),
        "Filename not sanitized correctly"
    );
}

#[tokio::test]
async fn rostra_media_empty_alt_uses_default() {
    let author_id =
        RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap();
    let content = format!("![](rostra-media:{TEST_EVENT_ID})");

    let html = render_with_images(&content, author_id).await;

    // Should use "media" as default display name
    assert!(
        html.contains(">media</a>"),
        "Missing default 'media' text in fallback"
    );
}

#[tokio::test]
async fn external_image_gets_lazy_loading() {
    let author_id =
        RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap();
    let content = "![alt text](https://example.com/image.png)";

    let html = render_with_images(content, author_id).await;

    // Should have lazyload wrapper
    assert!(
        html.contains("lazyload-wrapper"),
        "Missing lazyload wrapper"
    );

    // Should have lazyload message
    assert!(
        html.contains("lazyload-message"),
        "Missing lazyload message"
    );

    // Should use data-src instead of src for lazy loading
    assert!(
        html.contains("data-src=\"https://example.com/image.png\""),
        "Missing data-src attribute"
    );

    // Should include the alt text in the load message
    assert!(
        html.contains("alt text"),
        "Missing alt text in load message"
    );
}

#[tokio::test]
async fn youtube_embed_gets_lazy_loading() {
    let author_id =
        RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap();
    let content = "![video](https://www.youtube.com/watch?v=dQw4w9WgXcQ)";

    let html = render_with_images(content, author_id).await;

    // Should have lazyload wrapper
    assert!(
        html.contains("lazyload-wrapper"),
        "Missing lazyload wrapper"
    );

    // Should have video-specific lazyload message
    assert!(
        html.contains("lazyload-message -video"),
        "Missing video lazyload message"
    );

    // Should create an iframe with youtube embed URL
    assert!(
        html.contains("youtube.com/embed/dQw4w9WgXcQ"),
        "Missing youtube embed URL"
    );

    // Should use data-src for lazy loading
    assert!(
        html.contains("data-src=\"https://www.youtube.com/embed/"),
        "Missing data-src on iframe"
    );
}

#[tokio::test]
async fn youtu_be_short_url_works() {
    let author_id =
        RostraId::from_str("rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy").unwrap();
    let content = "![](https://youtu.be/dQw4w9WgXcQ)";

    let html = render_with_images(content, author_id).await;

    // Should create an iframe with youtube embed URL
    assert!(
        html.contains("youtube.com/embed/dQw4w9WgXcQ"),
        "Missing youtube embed URL for short URL"
    );
}

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
