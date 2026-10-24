use std::sync::Arc;

use rostra_core::event::{
    Event, EventAuxKey, EventContentKind as _, EventExt as _, EventKind, SocialMedia,
    VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::static_media::normalize_static_media;

use crate::media_request::{MediaRequest, MediaRequestId};
use crate::{Database, DbError};

fn request() -> MediaRequest {
    let image = normalize_static_media(
        "image/webp",
        include_bytes!("../../rostra-core/src/avatar/fixtures/red-3x2.webp"),
    )
    .unwrap();
    MediaRequest {
        mime: "image/png".into(),
        data: image.data,
        policy_version: 1,
    }
}

fn candidate(secret: RostraIdSecretKey, request: &MediaRequest, nonce: u8) -> VerifiedEventContent {
    let raw = SocialMedia {
        mime: request.mime.clone(),
        data: request.data.clone(),
    }
    .serialize_cbor()
    .unwrap();
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_MEDIA)
        .content(&raw)
        .build();
    event.key_aux = EventAuxKey::from_bytes([nonce; 16]);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap(),
        raw,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn media_admission_refusal_commits_neither_header_nor_receipt() -> anyhow::Result<()> {
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
    let input = request();
    let content = candidate(secret, &input, 1);
    let event_id = content.event_id();
    let mut allocation = db.reserve_payload_allocation(u64::from(content.content_len()))?;
    let mut buffer = None;
    assert!(matches!(
        db.commit_media_request(
            MediaRequestId([1; 16]),
            &input,
            &content,
            &mut allocation,
            &mut buffer
        )
        .await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    drop(content);
    drop(buffer);
    drop(allocation);
    assert!(
        db.media_receipt(MediaRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
    assert!(db.get_event(event_id).await.is_none());
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(db.payload_admission_usage().buffers, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn media_receipts_atomic_lost_ack_prune_delete_migrate_reopen() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("media.redb");
    let db = Database::open(&path, secret.id()).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::media_request_receipts::TABLE.as_raw())?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &39)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    let input = request();
    let id = MediaRequestId([1; 16]);
    let content = candidate(secret, &input, 1);
    assert!(
        db.commit_media_request_inner(
            id,
            &input,
            &content,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::MediaCandidate)))
        )
        .await
        .is_err()
    );
    assert!(db.get_event(content.event_id()).await.is_none());
    assert!(db.media_receipt(id, &input).await?.is_none());
    let task = tokio::spawn({
        let db = db.clone();
        let input = input.clone();
        let content = content.clone();
        async move {
            db.commit_media_request_inner(
                id,
                &input,
                &content,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("lost media acknowledgment"));
                    Ok(())
                })),
            )
            .await
        }
    });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(
        db.media_receipt(id, &input).await?.unwrap().event_id,
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
    db.commit_media_request(id, &input, &content, &mut None, &mut None)
        .await?;
    assert!(db.get_event_content(content.event_id()).await.is_none());
    assert!(db.list_own_media(None).await?.items.is_empty());
    let deletion = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_MEDIA)
        .delete(content.event_id().to_short())
        .build()
        .signed_by(secret);
    db.try_process_event(&VerifiedEvent::verify_signed(secret.id(), deletion)?)
        .await?;
    db.commit_media_request(id, &input, &content, &mut None, &mut None)
        .await?;
    assert!(db.get_event_content(content.event_id()).await.is_none());
    db.write_with(|tx| Database::prepare_total_migration(tx, 40))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        db.media_receipt(id, &input).await?.unwrap().event_id,
        content.event_id()
    );
    assert!(db.get_event_content(content.event_id()).await.is_none());
    let extension = crate::ExtensionTableDefinition::<u64, u64>::new("media_request_receipts");
    assert!(
        db.extension_write(|tx| {
            tx.open_table(&extension)?;
            Ok(())
        })
        .await
        .is_err()
    );
    let mut changed_inputs = [input.clone(), input.clone(), input.clone()];
    changed_inputs[0].data.push(0);
    changed_inputs[1].mime = "image/webp".into();
    changed_inputs[2].policy_version = 2;
    for changed in changed_inputs {
        assert!(matches!(
            db.media_receipt(id, &changed).await,
            Err(DbError::MediaRequestMismatch)
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn media_candidate_listing_scan_progress_lifecycle_corruption_and_cursor_binding()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let input = request();
    let mut expected = std::collections::BTreeSet::new();
    for n in 1..=25 {
        let content = candidate(secret, &input, n);
        db.commit_media_request(
            MediaRequestId([n; 16]),
            &input,
            &content,
            &mut None,
            &mut None,
        )
        .await?;
        expected.insert(content.event_id());
    }
    let first = db.list_own_media(None).await?;
    assert!(first.next_cursor.is_some());
    let cursor = first.next_cursor.clone().unwrap();
    let other = Database::new_in_memory(secret.id()).await?;
    assert!(matches!(
        other.list_own_media(Some(&cursor)).await,
        Err(DbError::MediaCursorStale)
    ));
    let mut found = std::collections::BTreeSet::new();
    let mut page = first;
    loop {
        assert!(page.raw_rows_scanned <= 128);
        assert!(page.bytes_reserved <= 8 * crate::native_media::MAX_NATIVE_MEDIA_BODY_BYTES);
        for item in page.items {
            assert_eq!((item.width, item.height, item.mime), (3, 2, "image/png"));
            assert!(found.insert(item.event_id));
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        page = db.list_own_media(Some(&cursor)).await?;
    }
    assert_eq!(found, expected);
    db.write_with(|tx| Database::prepare_total_migration(tx, 40))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert!(matches!(
        db.list_own_media(Some(&cursor)).await,
        Err(DbError::MediaCursorStale)
    ));
    // Unsupported payloads are skipped without stopping traversal.
    db.write_with(|tx| {
        let mut store = tx
            .as_raw()
            .open_table(crate::content_store::TABLE.as_raw())?;
        let hash = candidate(secret, &input, 1).content_hash();
        store.insert(
            bincode::encode_to_vec(hash, redb_bincode::BINCODE_CONFIG)
                .unwrap()
                .as_slice(),
            &[0xff][..],
        )?;
        Ok(())
    })
    .await?;
    let page = db.list_own_media(None).await?;
    assert!(page.items.is_empty());
    assert!(page.next_cursor.is_some());
    Ok(())
}
