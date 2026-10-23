use super::{EventContentKind as _, SocialPost};

#[test]
fn mutable_text_preserves_known_historical_fields_and_rejects_lossy_decodes() {
    // Minimal historical map, legacy persona preserved instead of its tag fallback.
    let historical = b"\xa3\x61p\x02\x61c\x61x\x61t\x80";
    let post = SocialPost::decode_mutable_text(historical).unwrap();
    let encoded = post.serialize_cbor().unwrap();
    assert_eq!(
        SocialPost::decode_mutable_text(encoded.as_ref()),
        Some(post)
    );
    // Indefinite map and chunked string are valid, not unknown metadata.
    assert!(SocialPost::decode_mutable_text(b"\xbf\x61c\x61x\xff").is_some());
    // The pinned decoder does not accept chunked strings here.
    assert!(SocialPost::decode_mutable_text(b"\xbf\x61c\x7f\x61x\x61y\xff\xff").is_none());
    for raw in [
        &b"\xa2\x61c\x61x\x61c\x61y"[..],         // Duplicate body.
        &b"\xa3\x61c\x61x\x61r\xf6\x61r\xf6"[..], // Duplicate optional null.
        &b"\xa2\x61c\x61x\x61z\x80"[..],          // Unknown metadata.
        &b"\xa1\x61c\x61x\x00"[..],               // Trailing object.
        &b"\xa1\x61c\x81\x61x"[..],               // Wrong body type.
        &b"\xa2\x61c\x61x\x61n\xf5"[..],          // News flag.
        &b"\xa2\x61c\x61x\x61h\x61y"[..],         // News title even without flag.
        &b"\xa2\x61c\x61x\x61e\x61y"[..],         // Reaction even with text.
        &b"\xa1\x61c\x7b\xff\xff\xff\xff\xff\xff\xff\xff"[..], // Huge length.
        &b"\x81\x61x"[..],                        // Not a map.
    ] {
        assert!(SocialPost::decode_mutable_text(raw).is_none(), "{raw:?}");
    }
    let mut deep = b"\xa2\x61c\x61x\x61z".to_vec();
    deep.extend(std::iter::repeat_n(0x81, 1024));
    assert!(SocialPost::decode_mutable_text(&deep).is_none());
    assert!(SocialPost::decode_mutable_text(&vec![0; 1_000_001]).is_none());
}
