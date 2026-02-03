//! Tests for URL protocol sanitization.
//!
//! These tests verify that dangerous URL protocols (javascript:, vbscript:,
//! data:) are blocked in links and images.

use super::render_sanitized;

/// Verify that javascript: URLs in djot links are sanitized.
/// The SanitizeUrls filter replaces them with "#".
#[tokio::test]
async fn javascript_url_in_link_is_sanitized() {
    let content = r#"[click me](javascript:alert('xss'))"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.contains(r#"href="javascript:"#),
        "javascript: URLs should be blocked in href attributes. Got: {html}"
    );
    // Should be replaced with #
    assert!(
        html.contains(r##"href="#""##),
        "dangerous URL should be replaced with #. Got: {html}"
    );
}

/// Verify that javascript: URLs in djot autolinks are sanitized.
/// The SanitizeUrls filter replaces them with "#".
#[tokio::test]
async fn autolink_javascript_is_sanitized() {
    let content = "<javascript:alert('xss')>";
    let html = render_sanitized(content).await;
    assert!(
        !html.contains(r#"href="javascript:"#),
        "javascript: URLs should be blocked in autolinks. Got: {html}"
    );
}

/// Verify that vbscript: URLs are also sanitized.
#[tokio::test]
async fn vbscript_url_is_sanitized() {
    let content = r#"[click me](vbscript:alert('xss'))"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.contains(r#"href="vbscript:"#),
        "vbscript: URLs should be blocked. Got: {html}"
    );
}

/// Verify that data: URLs are blocked (can be used for XSS).
#[tokio::test]
async fn data_url_in_link_is_sanitized() {
    let content = r#"[click me](data:text/html,<script>alert('xss')</script>)"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.contains(r#"href="data:"#),
        "data: URLs should be blocked. Got: {html}"
    );
}

/// Verify that ALL data: URLs are blocked, including seemingly safe ones.
/// We block all data: URLs out of caution.
#[tokio::test]
async fn all_data_urls_are_blocked() {
    // Even data:image URLs are blocked
    let content = r#"![img](data:image/png;base64,iVBORw0KGgo=)"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.contains("data:image"),
        "All data: URLs should be blocked, including images. Got: {html}"
    );
}
