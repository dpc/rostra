use std::sync::Arc;

use rostra_core::event::{
    Event, EventContentKind as _, EventExt as _, EventKind, SocialMedia, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use crate::public_attachment_content::PublicAttachmentContent;
use crate::public_attachment_request::{
    PUBLIC_ATTACHMENT_MAX_BYTES, PUBLIC_ATTACHMENT_MIME, PublicAttachmentRequest,
    PublicAttachmentRequestId,
};
use crate::{Database, DbError};

fn candidate(
    secret: RostraIdSecretKey,
    id: PublicAttachmentRequestId,
    request: &PublicAttachmentRequest,
) -> VerifiedEventContent {
    let raw = PublicAttachmentContent::new(request)
        .serialize_cbor()
        .unwrap();
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .timestamp(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(123))
        .kind(EventKind::SOCIAL_MEDIA)
        .content(&raw)
        .build();
    event.key_aux = id.event_aux_key(secret.id());
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap(),
        raw,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachment_exact_wire_boundaries_and_candidate_binding() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for (n, data) in [
        vec![0],
        (0..=255).collect(),
        vec![255; PUBLIC_ATTACHMENT_MAX_BYTES],
    ]
    .into_iter()
    .enumerate()
    {
        let request = PublicAttachmentRequest { data };
        let id = PublicAttachmentRequestId([n as u8; 16]);
        let content = candidate(secret, id, &request);
        let legacy = SocialMedia {
            mime: PUBLIC_ATTACHMENT_MIME.to_owned(),
            data: request.data.clone(),
        }
        .serialize_cbor()?;
        assert_eq!(content.content.as_ref().unwrap().as_ref(), legacy.as_ref());
        assert!(content.content_len() <= 2_000_128);
        assert!(!content.is_singleton());
        db.commit_public_attachment_request(id, &request, &content, &mut None, &mut None)
            .await?;
        #[cfg(feature = "native-media")]
        {
            use rostra_core::media::MediaEventReference;

            use crate::native_media::{NativeMediaRead, OpaqueMediaRead};
            let reference = MediaEventReference::Full(content.event_id());
            assert_eq!(
                db.read_native_media_opaque(secret.id(), reference).await?,
                OpaqueMediaRead::Available {
                    author: secret.id(),
                    event_id: content.event_id(),
                    mime: PUBLIC_ATTACHMENT_MIME.to_owned(),
                    data: request.data.clone(),
                }
            );
            assert_eq!(
                db.read_native_media(secret.id(), reference).await?,
                NativeMediaRead::Unsupported
            );
        }
    }
    let request = PublicAttachmentRequest { data: vec![1, 255] };
    let id = PublicAttachmentRequestId([7; 16]);
    let a = candidate(secret, id, &request);
    let b = candidate(secret, PublicAttachmentRequestId([8; 16]), &request);
    assert_ne!(a.event_id(), b.event_id());
    assert_ne!(
        id.event_aux_key(secret.id()),
        id.event_aux_key(RostraIdSecretKey::generate().id())
    );
    assert_ne!(
        request.digest(secret.id()),
        request.digest(RostraIdSecretKey::generate().id())
    );
    for invalid in [
        PublicAttachmentRequest { data: vec![] },
        PublicAttachmentRequest {
            data: vec![0; PUBLIC_ATTACHMENT_MAX_BYTES + 1],
        },
    ] {
        assert!(matches!(
            db.commit_public_attachment_request(id, &invalid, &a, &mut None, &mut None)
                .await,
            Err(DbError::PublicAttachmentInput)
        ));
    }
    let wrong_bytes = PublicAttachmentRequest { data: vec![2, 255] };
    for wrong in [b, candidate(secret, id, &wrong_bytes)] {
        assert!(matches!(
            db.commit_public_attachment_request(id, &request, &wrong, &mut None, &mut None)
                .await,
            Err(DbError::PublicAttachmentCandidate)
        ));
        assert!(db.get_event(wrong.event_id()).await.is_none());
    }
    db.try_process_event_with_content(&a).await?;
    assert!(matches!(
        db.commit_public_attachment_request(id, &request, &a, &mut None, &mut None)
            .await,
        Err(DbError::PublicAttachmentCandidate)
    ));
    assert!(db.public_attachment_receipt(id, &request).await?.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachment_receipt_rollback_prune_delete_migration_and_reopen() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("attachments.redb");
    let db = Database::open(&path, secret.id()).await?;
    // A real schema-42 database has no attachment journal.
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::media_public_attachment_receipts::TABLE.as_raw())?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &42)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    let request = PublicAttachmentRequest {
        data: vec![0, 255, 128],
    };
    let id = PublicAttachmentRequestId([1; 16]);
    let content = candidate(secret, id, &request);
    assert!(
        db.commit_public_attachment_request_inner(
            id,
            &request,
            &content,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::PublicAttachmentCandidate))),
        )
        .await
        .is_err()
    );
    assert!(db.get_event(content.event_id()).await.is_none());
    assert!(db.public_attachment_receipt(id, &request).await?.is_none());
    let task = tokio::spawn({
        let db = db.clone();
        let request = request.clone();
        let content = content.clone();
        async move {
            db.commit_public_attachment_request_inner(
                id,
                &request,
                &content,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("lost attachment acknowledgment"));
                    Ok(())
                })),
            )
            .await
        }
    });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(
        db.public_attachment_receipt(id, &request)
            .await?
            .unwrap()
            .event_id,
        content.event_id()
    );
    db.write_with(|tx| {
        let before = db.payload_before_tx(tx, &content.event)?;
        Database::prune_event_content_tx(
            content.event_id(),
            content.content_hash(),
            &mut tx.open_table(&crate::events_content_state::TABLE)?,
            &mut tx.open_table(&crate::content_rc::TABLE)?,
            &mut tx.open_table(&crate::events_content_missing::TABLE)?,
            Some((
                content.author(),
                content.content_len(),
                &mut tx.open_table(&crate::ids_data_usage::TABLE)?,
            )),
        )?;
        db.payload_after_tx(tx, before)
    })
    .await?;
    db.commit_public_attachment_request(id, &request, &content, &mut None, &mut None)
        .await?;
    assert!(db.get_event_content(content.event_id()).await.is_none());
    let deletion = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_MEDIA)
        .delete(content.event_id().to_short())
        .build()
        .signed_by(secret);
    db.try_process_event(&VerifiedEvent::verify_signed(secret.id(), deletion)?)
        .await?;
    // The original receipt resolves even with a now-invalid supplied candidate.
    let wrong = candidate(RostraIdSecretKey::generate(), id, &request);
    assert_eq!(
        db.commit_public_attachment_request(id, &request, &wrong, &mut None, &mut None)
            .await?
            .event_id,
        content.event_id()
    );
    assert!(db.get_event_content(content.event_id()).await.is_none());
    db.write_with(|tx| Database::prepare_total_migration(tx, 43))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        db.public_attachment_receipt(id, &request)
            .await?
            .unwrap()
            .event_id,
        content.event_id()
    );
    assert!(db.get_event_content(content.event_id()).await.is_none());
    let changed = PublicAttachmentRequest { data: vec![] };
    assert!(matches!(
        db.public_attachment_receipt(id, &changed).await,
        Err(DbError::PublicAttachmentRequestMismatch)
    ));
    let extension =
        crate::ExtensionTableDefinition::<u64, u64>::new("media_public_attachment_receipts");
    assert!(
        db.extension_write(|tx| {
            tx.open_table(&extension)?;
            Ok(())
        })
        .await
        .is_err()
    );
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&crate::db_version::TABLE)?
                .get(&())?
                .unwrap()
                .value(),
            43
        );
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachment_admission_failure_leaves_no_event_or_receipt() -> anyhow::Result<()> {
    use std::num::{NonZeroU64, NonZeroUsize};

    use crate::{PayloadAdmissionConfig, PayloadAdmissionLimits};
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    let config = PayloadAdmissionConfig::new(PayloadAdmissionLimits {
        database_bytes: NonZeroU64::new(1).unwrap(),
        author_bytes: NonZeroU64::new(1).unwrap(),
        overrides: Default::default(),
        in_flight_count: NonZeroUsize::new(4).unwrap(),
        in_flight_bytes: NonZeroU64::new(10000).unwrap(),
    })
    .unwrap();
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(config);
        Ok(())
    })
    .await?;
    let request = PublicAttachmentRequest { data: vec![0, 255] };
    let id = PublicAttachmentRequestId([1; 16]);
    let content = candidate(secret, id, &request);
    let mut allocation = db.reserve_payload_allocation(u64::from(content.content_len()))?;
    let mut buffer = None;
    assert!(matches!(
        db.commit_public_attachment_request(id, &request, &content, &mut allocation, &mut buffer,)
            .await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(db.public_attachment_receipt(id, &request).await?.is_none());
    assert!(db.get_event(content.event_id()).await.is_none());
    assert!(db.get_event_content(content.event_id()).await.is_none());
    Ok(())
}
