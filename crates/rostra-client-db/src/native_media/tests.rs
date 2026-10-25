//! Local-only integration fixtures; never contact a peer or use live
//! identities.

use std::borrow::Cow;

use rostra_core::event::content_kind::{EventContentKind as _, SocialMedia};
use rostra_core::event::{Event, EventContentRaw, SignedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;

use super::*;
use crate::event::{ContentStoreRecord, EventContentState};

mod opaque;

// Complete grayscale 3x2 PNG, with no external fixture/package dependency.
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 3, 0, 0, 0, 2, 8, 0, 0,
    0, 0, 184, 31, 57, 198, 0, 0, 0, 14, 73, 68, 65, 84, 120, 156, 99, 80, 85, 85, 101, 0, 98, 0,
    3, 128, 0, 223, 160, 152, 197, 200, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

// Complete lossy 3x2 WebP, shared image content with the core validator
// fixture.
const WEBP: &[u8] = &[
    82, 73, 70, 70, 60, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 32, 48, 0, 0, 0, 208, 1, 0, 157, 1,
    42, 3, 0, 2, 0, 2, 0, 52, 37, 160, 2, 116, 186, 1, 248, 0, 3, 176, 0, 254, 240, 196, 11, 255,
    32, 185, 97, 117, 200, 215, 255, 32, 63, 228, 7, 252, 128, 255, 248, 242, 0, 0, 0,
];

fn body(mime: &str, data: &[u8]) -> EventContentRaw {
    SocialMedia {
        mime: mime.into(),
        data: data.into(),
    }
    .serialize_cbor()
    .unwrap()
}

fn signed(secret: RostraIdSecretKey, body: &EventContentRaw, kind: EventKind) -> SignedEvent {
    Event::builder_raw_content()
        .author(secret.id())
        .kind(kind)
        .content(body)
        .build()
        .signed_by(secret)
}

async fn retain(db: &Database, event: SignedEvent, body: EventContentRaw) -> anyhow::Result<()> {
    db.write_with(|tx| {
        tx.open_table(&crate::events::TABLE)?
            .insert(&event.compute_short_id(), &EventRecord { signed: event })?;
        tx.open_table(&crate::content_store::TABLE)?
            .insert(&event.content_hash(), &ContentStoreRecord(Cow::Owned(body)))?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_ingestion_and_offline_reads_preserve_bytes_and_bind_exact_identity()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("image/png", PNG);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    let verified =
        VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
    db.try_process_event_with_content(&verified).await?;
    let expected = NativeMediaRead::Available {
        author: secret.id(),
        event_id: event.compute_id(),
        mime: "image/png",
        width: 3,
        height: 2,
        data: PNG.into(),
    };
    for reference in [
        MediaEventReference::Full(event.compute_id()),
        MediaEventReference::Short(event.compute_short_id()),
    ] {
        assert_eq!(
            db.read_native_media(secret.id(), reference).await?,
            expected
        );
        assert_eq!(
            reference
                .to_string()
                .parse::<MediaEventReference>()
                .unwrap(),
            reference
        );
    }
    assert_eq!(
        db.read_native_media(
            RostraIdSecretKey::generate().id(),
            MediaEventReference::Full(event.compute_id())
        )
        .await?,
        NativeMediaRead::Mismatch
    );
    let mut collision: [u8; 32] = event.compute_id().into();
    collision[31] ^= 1;
    assert_eq!(
        db.read_native_media(
            secret.id(),
            MediaEventReference::Full(EventId::from_bytes(collision))
        )
        .await?,
        NativeMediaRead::Mismatch
    );
    let wrong_kind = signed(secret, &body("image/png", PNG), EventKind::SOCIAL_POST);
    retain(&db, wrong_kind, body("image/png", PNG)).await?;
    assert_eq!(
        db.read_native_media(
            secret.id(),
            MediaEventReference::Full(wrong_kind.compute_id())
        )
        .await?,
        NativeMediaRead::Mismatch
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_is_checked_before_shared_content_and_reads_never_schedule_fetches()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("image/png", PNG);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    let reference = MediaEventReference::Full(event.compute_id());
    assert_eq!(
        db.read_native_media(secret.id(), reference).await?,
        NativeMediaRead::Missing
    );
    retain(&db, event, raw).await?;
    let missing = EventContentState::Missing {
        last_fetch_attempt: None,
        fetch_attempt_count: 7,
        next_fetch_attempt: 123.into(),
    };
    for (state, expected) in [
        (missing, NativeMediaRead::Missing),
        (
            EventContentState::Deleted {
                deleted_by: event.compute_short_id(),
            },
            NativeMediaRead::Deleted,
        ),
        (EventContentState::Pruned, NativeMediaRead::Pruned),
        (EventContentState::Invalid, NativeMediaRead::Invalid),
    ] {
        db.write_with(|tx| {
            tx.open_table(&crate::events_content_state::TABLE)?
                .insert(&event.compute_short_id(), &state)?;
            Ok(())
        })
        .await?;
        for _ in 0..2 {
            assert_eq!(
                db.read_native_media(secret.id(), reference).await?,
                expected
            );
            assert_eq!(
                validate(db.read_native_media_opaque(secret.id(), reference).await?),
                expected
            );
        }
        db.read_with(|tx| {
            assert_eq!(
                tx.open_table(&crate::events_content_state::TABLE)?
                    .get(&event.compute_short_id())?
                    .unwrap()
                    .value(),
                state
            );
            Ok(())
        })
        .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_storage_cannot_substitute_media_or_force_unbounded_allocation()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("image/png", PNG);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    let reference = MediaEventReference::Full(event.compute_id());
    retain(&db, event, raw.clone()).await?;
    let key = bincode::encode_to_vec(event.content_hash(), redb_bincode::BINCODE_CONFIG)?;
    let original = bincode::encode_to_vec(
        ContentStoreRecord(Cow::Owned(raw)),
        redb_bincode::BINCODE_CONFIG,
    )?;
    let mut trailing = original.clone();
    trailing.push(0);
    let mut substituted = original.clone();
    *substituted.last_mut().unwrap() ^= 1;
    // Huge forged bincode Vec length, too little data: borrowed decode must fail,
    // not reserve the attacker-declared length.
    let mut forged = vec![253];
    forged.extend_from_slice(&u64::MAX.to_le_bytes());
    for (bytes, expected) in [
        (substituted, NativeMediaRead::Invalid),
        (trailing, NativeMediaRead::Invalid),
        (forged, NativeMediaRead::Invalid),
        (
            vec![0; MAX_NATIVE_MEDIA_BODY_BYTES + 10],
            NativeMediaRead::TooLarge,
        ),
    ] {
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::content_store::TABLE.as_raw())?
                .insert(key.as_slice(), bytes.as_slice())?;
            Ok(())
        })
        .await?;
        assert_eq!(
            db.read_native_media(secret.id(), reference).await?,
            expected
        );
        assert_eq!(
            validate(db.read_native_media_opaque(secret.id(), reference).await?),
            expected
        );
    }
    // Correct envelope hash but corrupt signature cannot authorize the payload.
    let other = signed(
        RostraIdSecretKey::generate(),
        &body("image/png", PNG),
        EventKind::SOCIAL_MEDIA,
    );
    let invalid = SignedEvent::unverified(*event.event(), other.sig());
    retain(&db, invalid, body("image/png", PNG)).await?;
    assert_eq!(
        db.read_native_media(secret.id(), reference).await?,
        NativeMediaRead::Invalid
    );
    assert_eq!(
        db.read_native_media_opaque(secret.id(), reference).await?,
        OpaqueMediaRead::Invalid
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn body_byte_media_and_image_policies_remain_distinct() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let mut animated = PNG.to_vec();
    let iend = animated.len() - 12;
    animated.splice(iend..iend, [0, 0, 0, 0, b'a', b'c', b'T', b'L', 0, 0, 0, 0]);
    let webp = body("image/webp", WEBP);
    let event = signed(secret, &webp, EventKind::SOCIAL_MEDIA);
    retain(&db, event, webp).await?;
    assert_eq!(
        db.read_native_media(secret.id(), MediaEventReference::Full(event.compute_id()))
            .await?,
        NativeMediaRead::Available {
            author: secret.id(),
            event_id: event.compute_id(),
            mime: "image/webp",
            width: 3,
            height: 2,
            data: WEBP.into(),
        }
    );
    let mut dimensions = WEBP.to_vec();
    dimensions[26..28].copy_from_slice(&2049u16.to_le_bytes());
    let mut pixels = WEBP.to_vec();
    pixels[26..28].copy_from_slice(&2048u16.to_le_bytes());
    pixels[28..30].copy_from_slice(&2048u16.to_le_bytes());
    let mut animated_webp = WEBP.to_vec();
    animated_webp.extend_from_slice(b"ANIM\0\0\0\0");
    let riff_len = (animated_webp.len() - 8) as u32;
    animated_webp[4..8].copy_from_slice(&riff_len.to_le_bytes());
    for (raw, expected) in [
        (body("image/webp", &dimensions), NativeMediaRead::TooLarge),
        (body("image/webp", &pixels), NativeMediaRead::TooLarge),
        (
            body("image/webp", &animated_webp),
            NativeMediaRead::Unsupported,
        ),
        (
            body("image/png; charset=binary", PNG),
            NativeMediaRead::Unsupported,
        ),
        (body("image/webp", PNG), NativeMediaRead::Invalid),
        (
            body("image/png", &PNG[..PNG.len() - 1]),
            NativeMediaRead::Invalid,
        ),
        (body("image/png", &animated), NativeMediaRead::Unsupported),
        (
            body("image/png", &vec![0; 1_000_001]),
            NativeMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(vec![0; MAX_NATIVE_MEDIA_BODY_BYTES + 1]),
            NativeMediaRead::TooLarge,
        ),
    ] {
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain(&db, event, raw).await?;
        assert_eq!(
            db.read_native_media(secret.id(), MediaEventReference::Full(event.compute_id()))
                .await?,
            expected
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn retained_metadata_survives_presentation_byte_for_byte() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let mut png = PNG.to_vec();
    // Valid tEXt chunk, not an instruction to normalize or strip metadata.
    png.splice(
        33..33,
        [
            0, 0, 0, 35, 116, 69, 88, 116, 67, 111, 109, 109, 101, 110, 116, 0, 114, 101, 116, 97,
            105, 110, 101, 100, 32, 117, 110, 116, 114, 117, 115, 116, 101, 100, 32, 109, 101, 116,
            97, 100, 97, 116, 97, 217, 116, 109, 145,
        ],
    );
    let raw = body("image/png", &png);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, raw).await?;
    assert_eq!(
        db.read_native_media(secret.id(), MediaEventReference::Full(event.compute_id()))
            .await?,
        NativeMediaRead::Available {
            author: secret.id(),
            event_id: event.compute_id(),
            mime: "image/png",
            width: 3,
            height: 2,
            data: png,
        }
    );
    Ok(())
}

#[test]
fn cbor_preflight_rejects_nested_duplicate_trailing_and_noncanonical_values() {
    let canonical = body("image/png", PNG);
    assert_eq!(
        payload::decode(canonical.as_ref(), true).unwrap(),
        ("image/png", PNG.to_vec())
    );
    // All truncated prefixes fail before image bytes are returned.
    for end in 0..canonical.len() {
        for image_only in [false, true] {
            assert!(payload::decode(&canonical.as_ref()[..end], image_only).is_err());
        }
    }
    for bytes in [
        b"\xa2\x61m\x69image/png\x61m\x69image/png".as_slice(),
        b"\xa2\x61d\x80\x61d\x80".as_slice(),
        b"\xa2\x61m\x69image/png\x61d\x81\x81\x00".as_slice(),
        b"\xa2\x61m\x69image/png\x61d\x81\x18\x00".as_slice(),
        b"\xa2\x61m\x69image/png\x61d\x9b\xff\xff\xff\xff\xff\xff\xff\xff".as_slice(),
        b"\xa2\x61m\x69image/png\x61x\x80".as_slice(),
        b"\xa2\x61m\x69image/png\x61d\x40".as_slice(),
        b"\xbf\x61m\x69image/png\x61d\x80\xff".as_slice(),
        b"\xa2\x61m\x69image/png\x61d\xc0\x80".as_slice(),
        b"\xa2\x61m\x69image/png\x61d\x80\x00".as_slice(),
    ] {
        for image_only in [false, true] {
            assert!(payload::decode(bytes, image_only).is_err(), "{bytes:?}");
        }
    }
    assert_eq!(
        payload::decode(b"\xa2\x61d\x80\x61m\x69image/png", true).unwrap(),
        ("image/png", vec![])
    );
    // The maximum canonical integer representation fits the body policy.
    let maximum = body("image/png", &vec![255; 1_000_000]);
    assert!(maximum.len() <= MAX_NATIVE_MEDIA_BODY_BYTES);
    assert_eq!(
        payload::decode(maximum.as_ref(), true).unwrap().1.len(),
        1_000_000
    );
}
