use super::media_html;
use crate::routes::media_type::verify_browser_media;

#[test]
fn mismatched_image_uses_download_markup() {
    let html = media_html(
        verify_browser_media("image/png", b"<script>alert(1)</script>"),
        "/media/test",
        "hostile",
        "hostile",
    );

    assert!(html.contains("m-rostraMedia__download"));
    assert!(!html.contains("<img"));
}
