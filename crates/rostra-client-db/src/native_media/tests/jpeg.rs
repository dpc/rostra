use super::*;

// Same public synthetic zero-coefficient control retained by the private
// helper.
const JPEG: &[u8] = include_bytes!("jpeg-420-17x15.jpg");

#[tokio::test(flavor = "multi_thread")]
async fn explicit_jpeg_read_and_list_preserve_default_policy_and_identity() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let raw = body("image/jpeg", JPEG);
    let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
    let verified =
        VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
    db.try_process_event_with_content(&verified).await?;
    let reference = MediaEventReference::Full(event.compute_id());
    assert_eq!(
        db.read_native_media(secret.id(), reference).await?,
        NativeMediaRead::Unsupported
    );
    assert!(db.list_own_media(None).await?.items.is_empty());
    let page = db.list_own_media_with_jpeg(None).await?;
    assert_eq!(page.items.len(), 1);
    let candidate = &page.items[0];
    assert_eq!(
        (candidate.width, candidate.height, candidate.bytes),
        (17, 15, JPEG.len())
    );
    assert_eq!(
        candidate.eligibility,
        OwnMediaEligibility::RequiresReadValidation
    );
    assert_eq!(candidate.event_id, event.compute_id());
    assert_eq!(
        db.read_native_media_with_jpeg(secret.id(), reference)
            .await?,
        NativeMediaRead::Available {
            author: secret.id(),
            event_id: event.compute_id(),
            mime: "image/jpeg",
            width: 17,
            height: 15,
            data: JPEG.into(),
        }
    );
    assert_eq!(
        db.read_native_media_with_jpeg(RostraIdSecretKey::generate().id(), reference)
            .await?,
        NativeMediaRead::Mismatch
    );
    let mut collision: [u8; 32] = event.compute_id().into();
    collision[31] ^= 1;
    assert_eq!(
        db.read_native_media_with_jpeg(
            secret.id(),
            MediaEventReference::Full(EventId::from(blake3::Hash::from(collision)))
        )
        .await?,
        NativeMediaRead::Mismatch
    );
    db.write_with(|tx| {
        tx.open_table(&crate::events_content_state::TABLE)?
            .insert(&event.compute_short_id(), &EventContentState::Pruned)?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.read_native_media_with_jpeg(secret.id(), reference)
            .await?,
        NativeMediaRead::Pruned
    );
    assert!(db.list_own_media_with_jpeg(None).await?.items.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn candidate_is_not_entropy_validation_and_existing_rasters_still_work() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let sos = JPEG.windows(2).position(|b| b == [255, 218]).unwrap();
    let end = sos + 2 + usize::from(u16::from_be_bytes([JPEG[sos + 2], JPEG[sos + 3]]));
    let mut empty = JPEG[..end].to_vec();
    empty.extend([255, 217]);
    for (mime, bytes, valid) in [
        ("image/jpeg", empty.as_slice(), false),
        ("image/png", PNG, true),
        ("image/webp", WEBP, true),
    ] {
        let raw = body(mime, bytes);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        let verified =
            VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
        db.try_process_event_with_content(&verified).await?;
        let result = db
            .read_native_media_with_jpeg(secret.id(), MediaEventReference::Full(event.compute_id()))
            .await?;
        if valid {
            assert!(matches!(result, NativeMediaRead::Available { data, .. } if data == bytes));
        } else {
            assert_eq!(result, NativeMediaRead::Invalid);
        }
    }
    assert_eq!(db.list_own_media_with_jpeg(None).await?.items.len(), 3);
    assert_eq!(db.list_own_media(None).await?.items.len(), 2);
    Ok(())
}
