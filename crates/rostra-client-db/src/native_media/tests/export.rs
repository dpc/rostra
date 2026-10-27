//! Larger export remains a local, authenticated whole-buffer read.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn export_boundaries_preserve_default_and_image_limits() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for len in [1_000_001, MAX_NATIVE_MEDIA_EXPORT_BYTES] {
        let mime = "é".repeat(32);
        let data = vec![255; len];
        let raw = body(&mime, &data);
        assert_eq!(raw.len(), 2 * len + 76);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        let verified =
            VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
        db.try_process_event_with_content(&verified).await?;
        for reference in [
            MediaEventReference::Full(event.compute_id()),
            MediaEventReference::Short(event.compute_short_id()),
        ] {
            assert_eq!(
                db.read_native_media_opaque_export(secret.id(), reference)
                    .await?,
                OpaqueMediaRead::Available {
                    author: secret.id(),
                    event_id: event.compute_id(),
                    mime: mime.clone(),
                    data: data.clone(),
                }
            );
            assert_eq!(
                db.read_native_media_opaque(secret.id(), reference).await?,
                OpaqueMediaRead::TooLarge
            );
            assert_eq!(
                db.read_native_media(secret.id(), reference).await?,
                NativeMediaRead::TooLarge
            );
        }
    }
    for (mime, data) in [("image/png", PNG), ("image/webp", WEBP)] {
        let raw = body(mime, data);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain(&db, event, raw).await?;
        let reference = MediaEventReference::Full(event.compute_id());
        assert_eq!(
            db.read_native_media_opaque_export(secret.id(), reference)
                .await?,
            db.read_native_media_opaque(secret.id(), reference).await?
        );
        assert!(matches!(
            db.read_native_media(secret.id(), reference).await?,
            NativeMediaRead::Available { .. }
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn export_rejects_oversize_and_complete_cbor_failures() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for (raw, expected) in [
        (
            body("", &vec![0; MAX_NATIVE_MEDIA_EXPORT_BYTES + 1]),
            OpaqueMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(vec![0; MAX_NATIVE_MEDIA_EXPORT_BODY_BYTES + 1]),
            OpaqueMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(
                b"\xa2\x61m\x60\x61d\x9b\xff\xff\xff\xff\xff\xff\xff\xff".to_vec(),
            ),
            OpaqueMediaRead::TooLarge,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61d\x81".to_vec()),
            OpaqueMediaRead::Invalid,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61d\x81\x18".to_vec()),
            OpaqueMediaRead::Invalid,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61d\x81\x18\x00".to_vec()),
            OpaqueMediaRead::Unsupported,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x60\x61d\x40".to_vec()),
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
            EventContentRaw::new(b"\xa2\x61d\x80\x61d\x80".to_vec()),
            OpaqueMediaRead::Invalid,
        ),
    ] {
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain(&db, event, raw).await?;
        assert_eq!(
            db.read_native_media_opaque_export(
                secret.id(),
                MediaEventReference::Full(event.compute_id())
            )
            .await?,
            expected
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn export_checks_signed_length_before_opening_body_table() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = EventContentRaw::new(vec![0; MAX_NATIVE_MEDIA_EXPORT_BODY_BYTES + 1]);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, raw).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::content_store::TABLE.as_raw())?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.read_native_media_opaque_export(
            secret.id(),
            MediaEventReference::Full(event.compute_id())
        )
        .await?,
        OpaqueMediaRead::TooLarge
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn export_checks_raw_storage_cap_and_borrowed_length() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("", b"opaque");
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, raw).await?;
    let key = bincode::encode_to_vec(event.content_hash(), redb_bincode::BINCODE_CONFIG)?;
    for (bytes, expected) in [
        (
            vec![0; MAX_NATIVE_MEDIA_EXPORT_BODY_BYTES + 10],
            OpaqueMediaRead::TooLarge,
        ),
        (
            vec![0; MAX_NATIVE_MEDIA_EXPORT_BODY_BYTES + 9],
            OpaqueMediaRead::Invalid,
        ),
        (
            vec![253, 255, 255, 255, 255, 255, 255, 255, 255],
            OpaqueMediaRead::Invalid,
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
            db.read_native_media_opaque_export(
                secret.id(),
                MediaEventReference::Full(event.compute_id())
            )
            .await?,
            expected
        );
    }
    Ok(())
}
