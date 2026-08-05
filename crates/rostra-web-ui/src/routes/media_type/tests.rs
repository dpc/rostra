use super::{MAX_INLINE_SVG_BYTES, VerifiedMedia, verify_avatar, verify_browser_media};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg"><path/></svg>"#;

#[test]
fn verifies_each_inline_format() {
    for (mime, bytes, expected) in [
        (
            "image/avif",
            b"\0\0\0\x10ftypavif\0\0\0\0".as_slice(),
            VerifiedMedia::Image("image/avif"),
        ),
        (
            "image/bmp",
            b"BM".as_slice(),
            VerifiedMedia::Image("image/bmp"),
        ),
        (
            "image/gif",
            b"GIF".as_slice(),
            VerifiedMedia::Image("image/gif"),
        ),
        (
            "image/jpeg",
            b"\xff\xd8\xff".as_slice(),
            VerifiedMedia::Image("image/jpeg"),
        ),
        ("image/png", PNG, VerifiedMedia::Image("image/png")),
        (
            "image/tiff",
            b"II*\0\0\0\0\0\0\0".as_slice(),
            VerifiedMedia::Image("image/tiff"),
        ),
        (
            "image/vnd.microsoft.icon",
            b"\0\0\x01\0".as_slice(),
            VerifiedMedia::Image("image/vnd.microsoft.icon"),
        ),
        (
            "image/webp",
            b"RIFF\0\0\0\0WEBP".as_slice(),
            VerifiedMedia::Image("image/webp"),
        ),
        ("image/svg+xml", SVG, VerifiedMedia::Image("image/svg+xml")),
        (
            "video/mp4",
            b"\0\0\0\x0cftypisom".as_slice(),
            VerifiedMedia::Video("video/mp4"),
        ),
        (
            "video/webm",
            b"\x1a\x45\xdf\xa3".as_slice(),
            VerifiedMedia::Video("video/webm"),
        ),
    ] {
        assert_eq!(verify_browser_media(mime, bytes), Some(expected));
    }
}

#[test]
fn accepts_mime_parameters_and_canonicalizes_ico() {
    assert_eq!(
        verify_browser_media("image/png; charset=binary", PNG),
        Some(VerifiedMedia::Image("image/png"))
    );
    assert_eq!(
        verify_browser_media("image/x-icon", b"\0\0\x01\0"),
        Some(VerifiedMedia::Image("image/vnd.microsoft.icon"))
    );
}

#[test]
fn rejects_mismatched_and_hostile_images() {
    assert!(verify_avatar("image/png", b"<script>alert(1)</script>").is_none());
    assert!(verify_avatar("image/svg+xml", PNG).is_none());
    assert!(verify_avatar("image/svg+xml", br#"<html><svg/></html>"#).is_none());
    assert!(
        verify_avatar(
            "image/svg+xml",
            br#"<!DOCTYPE svg SYSTEM "https://attacker.invalid/evil"><svg/>"#
        )
        .is_none()
    );
}

#[test]
fn rejects_oversized_svg() {
    let mut svg = b"<svg>".to_vec();
    svg.resize(MAX_INLINE_SVG_BYTES, b' ');
    svg.extend_from_slice(b"</svg>");

    assert!(verify_avatar("image/svg+xml", &svg).is_none());
}
