use std::collections::BTreeSet;

use super::discovered_tags::{self, PAYLOAD_BYTES_MAX, TagDiscoveryIssue};
use super::{EventContentKind as _, PersonaTag, SocialPost};

#[test]
fn discovered_tags_preserve_effective_legacy_and_exact_labels() {
    for (raw, expected) in [
        (&b"\xa1\x61p\x00"[..], vec!["personal"]),
        (&b"\xa1\x61p\x01"[..], vec!["professional"]),
        (&b"\xa2\x61p\x02\x61t\x80"[..], vec!["civic"]),
        (&b"\xa1\x61p\x18\xff"[..], vec![]),
        (&b"\xa0"[..], vec![]),
        (&b"\xa2\x61p\x00\x61t\x81\x61x"[..], vec!["x"]),
    ] {
        assert_eq!(
            discovered_tags::decode(raw).unwrap(),
            expected
                .into_iter()
                .map(|s| PersonaTag::new(s).unwrap())
                .collect()
        );
    }
    let tags = BTreeSet::from([
        PersonaTag::new(" spaces, retained ").unwrap(),
        PersonaTag::new("日本語").unwrap(),
        PersonaTag::new("a".repeat(32)).unwrap(),
    ]);
    let post = SocialPost::new_text("body".into(), None, tags.clone());
    assert_eq!(
        discovered_tags::decode(post.serialize_cbor().unwrap().as_ref()).unwrap(),
        tags
    );
}

#[test]
fn discovered_tags_reject_adversarial_input_without_partial_labels() {
    for raw in [
        &b"\xa2\x61p\x00\x61p\x01"[..], // duplicate
        &b"\xa1\x61z\xf6"[..],          // unknown
        &b"\xa1\x61t\x81\x60"[..],      // empty label
        &b"\xa1\x61t\x81\x61A"[..],     // serde bypasses PersonaTag validation
        &b"\xa1\x61t\x9b\xff\xff\xff\xff\xff\xff\xff\xff"[..],
        &b"\xa1\x61c\x7b\xff\xff\xff\xff\xff\xff\xff\xff"[..],
        &b"\xa1\x61c\x81\x81\x81\x81\x81\x81\x81\x81\x81\xf6"[..],
        &b"\xa1\x61t\x9f\x61x\xff"[..], // indefinite tags
        &b"\xbf\x61p\x00\xff"[..],      // indefinite map
        &b"\xa1\x61c\xc0\xf6"[..],      // CBOR semantic tag
        &b"\xa1\x61t\x81\x61\xff"[..],  // invalid UTF-8
        &b"\xa1\x61t\x81\x61x\x00"[..], // trailing bytes
        &b"\xa1\x61t\x81\x78\x21aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..],
    ] {
        assert_eq!(
            discovered_tags::decode(raw),
            Err(TagDiscoveryIssue::Unsupported),
            "{raw:?}"
        );
    }
    // Repeated labels count before BTreeSet deduplication.
    let mut repeated = b"\xa1\x61t\x99\x01\x01".to_vec();
    for _ in 0..257 {
        repeated.extend_from_slice(b"\x61x");
    }
    assert_eq!(
        discovered_tags::decode(&repeated),
        Err(TagDiscoveryIssue::Unsupported)
    );
    let mut excessive_items = b"\xa1\x61r\x99\x04\x01".to_vec();
    excessive_items.extend(std::iter::repeat_n(0, 1025));
    assert_eq!(
        discovered_tags::decode(&excessive_items),
        Err(TagDiscoveryIssue::Unsupported)
    );
    assert_eq!(
        discovered_tags::decode(&vec![0; PAYLOAD_BYTES_MAX + 1]),
        Err(TagDiscoveryIssue::Oversized)
    );
}
