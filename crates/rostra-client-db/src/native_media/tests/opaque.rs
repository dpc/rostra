//! Opaque export keeps authentication and bounds without image eligibility.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn opaque_bytes_and_untrusted_mime_survive_while_images_stay_restricted() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    // These are deliberately not complete codecs: opaque availability must
    // never imply a successful decoder or safe-to-open claim.
    for (mime, bytes, image_result) in [
        (
            "image/jpeg",
            b"\xff\xd8\xff\xe0opaque JPEG".as_slice(),
            NativeMediaRead::Unsupported,
        ),
        (
            "video/mp4",
            b"\0\0\0\x18ftypmp42opaque video".as_slice(),
            NativeMediaRead::Unsupported,
        ),
        (
            "application/octet-stream",
            &[0, 255, 128, 42],
            NativeMediaRead::Unsupported,
        ),
        ("image/png", &[0, 255, 128, 42], NativeMediaRead::Invalid),
        (
            "../untrusted\0\r\nmime",
            b"arbitrary".as_slice(),
            NativeMediaRead::Unsupported,
        ),
        ("", b"opaque".as_slice(), NativeMediaRead::Unsupported),
    ] {
        let raw = body(mime, bytes);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        let verified =
            VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
        db.try_process_event_with_content(&verified).await?;
        for reference in [
            MediaEventReference::Short(event.compute_short_id()),
            MediaEventReference::Full(event.compute_id()),
        ] {
            assert_eq!(
                db.read_native_media_opaque(secret.id(), reference).await?,
                OpaqueMediaRead::Available {
                    author: secret.id(),
                    event_id: event.compute_id(),
                    mime: mime.to_owned(),
                    data: bytes.to_vec(),
                }
            );
            assert_eq!(
                db.read_native_media(secret.id(), reference).await?,
                image_result
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn opaque_reads_require_exact_identity_and_kind() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("video/mp4", b"opaque");
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    let reference = MediaEventReference::Full(event.compute_id());
    assert_eq!(
        db.read_native_media_opaque(secret.id(), reference).await?,
        OpaqueMediaRead::Missing
    );
    retain(&db, event, raw.clone()).await?;
    assert_eq!(
        db.read_native_media_opaque(RostraIdSecretKey::generate().id(), reference)
            .await?,
        OpaqueMediaRead::Mismatch
    );
    let mut collision: [u8; 32] = event.compute_id().into();
    collision[31] ^= 1;
    let collision = MediaEventReference::Full(EventId::from(blake3::Hash::from(collision)));
    assert_eq!(collision.short_id(), reference.short_id());
    assert_eq!(
        db.read_native_media_opaque(secret.id(), collision).await?,
        OpaqueMediaRead::Mismatch
    );
    let other_kind = signed(secret, &raw, EventKind::SOCIAL_POST);
    retain(&db, other_kind, raw).await?;
    assert_eq!(
        db.read_native_media_opaque(
            secret.id(),
            MediaEventReference::Full(other_kind.compute_id())
        )
        .await?,
        OpaqueMediaRead::Mismatch
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn opaque_limits_and_complete_cbor_apply_before_returning_bytes() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let mime = "é".repeat(32);
    let bytes = vec![255; 1_000_000];
    let maximum = body(&mime, &bytes);
    assert!(maximum.len() <= MAX_NATIVE_MEDIA_BODY_BYTES);
    let event = signed(secret, &maximum, EventKind::SOCIAL_MEDIA);
    retain(&db, event, maximum).await?;
    assert_eq!(
        db.read_native_media_opaque(secret.id(), MediaEventReference::Full(event.compute_id()))
            .await?,
        OpaqueMediaRead::Available {
            author: secret.id(),
            event_id: event.compute_id(),
            mime,
            data: bytes
        }
    );
    for (raw, expected) in [
        (
            body(&"x".repeat(65), b"opaque"),
            OpaqueMediaRead::Unsupported,
        ),
        (
            body("video/mp4", &vec![0; 1_000_001]),
            OpaqueMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(vec![0; MAX_NATIVE_MEDIA_BODY_BYTES + 1]),
            OpaqueMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(
                b"\xa2\x61m\x60\x61d\x9b\xff\xff\xff\xff\xff\xff\xff\xff".to_vec(),
            ),
            OpaqueMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61d\x81\x81\x00".to_vec()),
            OpaqueMediaRead::Unsupported,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61d\x80\x00".to_vec()),
            OpaqueMediaRead::Invalid,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61m\x60".to_vec()),
            OpaqueMediaRead::Invalid,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x61\xff\x61d\x80".to_vec()),
            OpaqueMediaRead::Invalid,
        ),
    ] {
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain(&db, event, raw).await?;
        assert_eq!(
            db.read_native_media_opaque(secret.id(), MediaEventReference::Full(event.compute_id()))
                .await?,
            expected
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn opaque_envelope_and_lifecycle_corruption_fail_closed() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("video/mp4", b"opaque");
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, raw).await?;
    let reference = MediaEventReference::Full(event.compute_id());
    let key = bincode::encode_to_vec(event.compute_short_id(), redb_bincode::BINCODE_CONFIG)?;
    let original =
        bincode::encode_to_vec(EventRecord { signed: event }, redb_bincode::BINCODE_CONFIG)?;
    let mut trailing = original;
    trailing.push(0);
    for bytes in [trailing, vec![0; 1025], vec![]] {
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::events::TABLE.as_raw())?
                .insert(key.as_slice(), bytes.as_slice())?;
            Ok(())
        })
        .await?;
        assert_eq!(
            db.read_native_media_opaque(secret.id(), reference).await?,
            OpaqueMediaRead::Invalid
        );
    }
    retain(&db, event, body("video/mp4", b"opaque")).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(crate::events_content_state::TABLE.as_raw())?
            .insert(key.as_slice(), [0u8; 65].as_slice())?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.read_native_media_opaque(secret.id(), reference).await?,
        OpaqueMediaRead::Invalid
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn opaque_database_errors_are_not_media_unavailability() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("video/mp4", b"opaque");
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, raw).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::content_store::TABLE.as_raw())?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.read_native_media_opaque(secret.id(), MediaEventReference::Full(event.compute_id()))
            .await,
        Err(crate::DbError::Table { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_length_is_checked_before_even_opening_payload_storage() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = EventContentRaw::new(vec![0; MAX_NATIVE_MEDIA_BODY_BYTES + 1]);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, raw).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::content_store::TABLE.as_raw())?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.read_native_media_opaque(secret.id(), MediaEventReference::Full(event.compute_id()))
            .await?,
        OpaqueMediaRead::TooLarge
    );
    Ok(())
}
