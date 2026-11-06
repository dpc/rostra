use super::*;

const LARGE_PNG: &[u8] = include_bytes!("display-4096x2048.png");

#[tokio::test(flavor = "multi_thread")]
async fn display_is_not_reuse_and_preserves_identity_lifecycle_and_original() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("image/png", LARGE_PNG);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    let verified =
        VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
    db.try_process_event_with_content(&verified).await?;
    let reference = MediaEventReference::Full(event.compute_id());
    assert_eq!(
        db.read_native_media_for_display(secret.id(), reference)
            .await?,
        NativeMediaRead::Available {
            author: secret.id(),
            event_id: event.compute_id(),
            mime: "image/png",
            width: 4096,
            height: 2048,
            data: LARGE_PNG.into(),
        }
    );
    assert_eq!(
        db.read_native_media(secret.id(), reference).await?,
        NativeMediaRead::TooLarge
    );
    assert_eq!(
        db.read_native_media_with_jpeg(secret.id(), reference)
            .await?,
        NativeMediaRead::TooLarge
    );
    assert!(db.list_own_media(None).await?.items.is_empty());
    assert!(db.list_own_media_with_jpeg(None).await?.items.is_empty());
    assert_eq!(
        db.read_native_media_for_display(RostraIdSecretKey::generate().id(), reference)
            .await?,
        NativeMediaRead::Mismatch
    );
    let mut collision: [u8; 32] = event.compute_id().into();
    collision[31] ^= 1;
    assert_eq!(
        db.read_native_media_for_display(
            secret.id(),
            MediaEventReference::Full(EventId::from(blake3::Hash::from(collision))),
        )
        .await?,
        NativeMediaRead::Mismatch
    );
    for (state, expected) in [
        (
            EventContentState::Missing {
                last_fetch_attempt: None,
                fetch_attempt_count: 7,
                next_fetch_attempt: 123.into(),
            },
            NativeMediaRead::Missing,
        ),
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
        assert_eq!(
            db.read_native_media_for_display(secret.id(), reference)
                .await?,
            expected
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn display_rejects_corrupt_body_and_wrong_kind_before_decode() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("image/png", LARGE_PNG);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    retain(&db, event, body("image/png", PNG)).await?;
    assert_eq!(
        db.read_native_media_for_display(
            secret.id(),
            MediaEventReference::Full(event.compute_id())
        )
        .await?,
        NativeMediaRead::Invalid
    );
    let event = signed(secret, &raw, EventKind::SOCIAL_POST);
    retain(&db, event, raw).await?;
    assert_eq!(
        db.read_native_media_for_display(
            secret.id(),
            MediaEventReference::Full(event.compute_id())
        )
        .await?,
        NativeMediaRead::Mismatch
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn display_preserves_existing_formats_and_rejects_unvalidated_sources() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for (mime, bytes) in [
        ("image/png", PNG),
        ("image/webp", WEBP),
        (
            "image/jpeg",
            include_bytes!("jpeg-420-17x15.jpg").as_slice(),
        ),
        ("image/png", &LARGE_PNG[..LARGE_PNG.len() - 1]),
        ("image/webp", LARGE_PNG),
        ("image/png", &[0; 1_000_001]),
    ] {
        let raw = body(mime, bytes);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain(&db, event, raw).await?;
        let reference = MediaEventReference::Full(event.compute_id());
        assert_eq!(
            db.read_native_media_for_display(secret.id(), reference)
                .await?,
            db.read_native_media_with_jpeg(secret.id(), reference)
                .await?,
        );
    }
    Ok(())
}
