//! Attachment reuse authenticates bounded metadata and requires a fresh exact
//! read.

use std::collections::BTreeSet;

use super::*;
use crate::DbError;
use crate::public_attachment_request::{PUBLIC_ATTACHMENT_MAX_BYTES, PUBLIC_ATTACHMENT_MIME};

async fn retain_indexed(
    db: &Database,
    event: SignedEvent,
    raw: EventContentRaw,
) -> anyhow::Result<()> {
    retain(db, event, raw).await?;
    db.write_with(|tx| {
        tx.open_table(&crate::events_self::TABLE)?
            .insert(&event.compute_short_id(), &())?;
        Ok(())
    })
    .await?;
    Ok(())
}

fn assert_progress(page: &OwnPublicAttachmentPage) {
    assert!(page.raw_rows_scanned <= 8);
    assert!(page.items.len() <= 20);
    assert_eq!(
        page.bytes_reserved,
        page.raw_rows_scanned * MAX_NATIVE_MEDIA_BODY_BYTES
    );
    assert_eq!(page.skipped_rows + page.items.len(), page.raw_rows_scanned);
    assert_eq!(
        page.next_cursor.is_some(),
        page.stop == OwnPublicAttachmentStop::BudgetReached
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachments_ingestion_paging_and_exact_reuse_revalidation() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let mut expected = BTreeSet::new();
    for n in 0..25 {
        let raw = body(PUBLIC_ATTACHMENT_MIME, &[n, 255]);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        let verified =
            VerifiedEventContent::verify(VerifiedEvent::verify_signed(secret.id(), event)?, raw)?;
        db.try_process_event_with_content(&verified).await?;
        expected.insert(event.compute_id());
    }
    let mut found = BTreeSet::new();
    let mut page = db.list_own_public_attachments(None).await?;
    loop {
        assert_progress(&page);
        assert_eq!(page.skipped_rows, 0);
        for item in page.items {
            assert!(found.insert(item.event_id));
            assert_eq!((item.mime, item.bytes), (PUBLIC_ATTACHMENT_MIME, 2));
            assert!(matches!(
                db.read_native_media_opaque(secret.id(), MediaEventReference::Full(item.event_id))
                    .await?,
                OpaqueMediaRead::Available { author, event_id, mime, data }
                if author == secret.id() && event_id == item.event_id
                    && mime == item.mime && data.len() == item.bytes
            ));
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        page = db.list_own_public_attachments(Some(&cursor)).await?;
    }
    assert_eq!(found, expected);
    assert!(db.list_own_media(None).await?.items.is_empty());
    let event_id = *found.first().unwrap();
    let mut collision: [u8; 32] = event_id.into();
    collision[31] ^= 1;
    assert_eq!(
        db.read_native_media_opaque(
            secret.id(),
            MediaEventReference::Full(EventId::from(blake3::Hash::from(collision)))
        )
        .await?,
        OpaqueMediaRead::Mismatch
    );
    for (state, expected) in [
        (EventContentState::Pruned, OpaqueMediaRead::Pruned),
        (
            EventContentState::Deleted {
                deleted_by: event_id.into(),
            },
            OpaqueMediaRead::Deleted,
        ),
    ] {
        db.write_with(|tx| {
            tx.open_table(&crate::events_content_state::TABLE)?
                .insert(&event_id.into(), &state)?;
            Ok(())
        })
        .await?;
        assert_eq!(
            db.read_native_media_opaque(secret.id(), MediaEventReference::Full(event_id))
                .await?,
            expected
        );
        let mut page = db.list_own_public_attachments(None).await?;
        loop {
            assert_progress(&page);
            assert!(!page.items.iter().any(|item| item.event_id == event_id));
            let Some(cursor) = page.next_cursor else {
                break;
            };
            page = db.list_own_public_attachments(Some(&cursor)).await?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachments_empty_progress_and_cursor_lifetimes() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("attachments.redb");
    let db = Database::open(&path, secret.id()).await?;
    // Nonmedia own rows count against work; unrelated global events are not
    // scanned.
    for n in 0..17 {
        let raw = body(PUBLIC_ATTACHMENT_MIME, &[n]);
        let event = signed(secret, &raw, EventKind::SOCIAL_POST);
        retain_indexed(&db, event, raw.clone()).await?;
        let other = signed(RostraIdSecretKey::generate(), &raw, EventKind::SOCIAL_MEDIA);
        retain(&db, other, raw).await?;
    }
    let first = db.list_own_public_attachments(None).await?;
    assert_progress(&first);
    assert!(first.items.is_empty());
    assert_eq!(first.skipped_rows, 8);
    let cursor = first.next_cursor.unwrap();
    let second = db.list_own_public_attachments(Some(&cursor)).await?;
    assert_progress(&second);
    assert_eq!(second.skipped_rows, 8);
    let third = db
        .list_own_public_attachments(second.next_cursor.as_ref())
        .await?;
    assert_progress(&third);
    assert_eq!(third.skipped_rows, 1);
    assert_eq!(third.stop, OwnPublicAttachmentStop::Exhausted);
    for owner in [secret.id(), RostraIdSecretKey::generate().id()] {
        let other = Database::new_in_memory(owner).await?;
        assert!(matches!(
            other.list_own_public_attachments(Some(&cursor)).await,
            Err(DbError::MediaCursorStale)
        ));
    }
    db.write_with(|tx| {
        tx.open_table(&crate::social_notification_epoch::TABLE)?
            .insert(&(), &[42; 16])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.list_own_public_attachments(Some(&cursor)).await,
        Err(DbError::MediaCursorStale)
    ));
    let cursor = db
        .list_own_public_attachments(None)
        .await?
        .next_cursor
        .unwrap();
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert!(matches!(
        db.list_own_public_attachments(Some(&cursor)).await,
        Err(DbError::MediaCursorStale)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachments_exact_policy_and_complete_preflight() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    for (raw, accepted) in [
        (body(PUBLIC_ATTACHMENT_MIME, &[0]), true),
        (
            EventContentRaw::new(
                b"\xa2\x61d\x81\x18\xff\x61m\x78\x18application/octet-stream".to_vec(),
            ),
            true,
        ),
        (
            body(
                PUBLIC_ATTACHMENT_MIME,
                &vec![255; PUBLIC_ATTACHMENT_MAX_BYTES],
            ),
            true,
        ),
        (
            EventContentRaw::new(b"\xa2\x61m\x78\x18application/octet-stream\x61d\x80".to_vec()),
            false,
        ),
        (
            body(
                PUBLIC_ATTACHMENT_MIME,
                &vec![0; PUBLIC_ATTACHMENT_MAX_BYTES + 1],
            ),
            false,
        ),
        (
            EventContentRaw::new(vec![0; MAX_NATIVE_MEDIA_BODY_BYTES + 1]),
            false,
        ),
        (body("application/octet-stream;name=x", &[1]), false),
        (body("Application/Octet-Stream", &[1]), false),
        (body("image/png", PNG), false),
        (body("image/webp", WEBP), false),
        (body("video/mp4", &[1]), false),
        // Malformed/trailing and unsupported/nonminimal inputs remain distinct
        // reader outcomes, but all count as skipped in this candidate traversal.
        (
            EventContentRaw::new(b"\xa2\x61m\x78\x18application/octet-stream\x61d\x81".to_vec()),
            false,
        ),
        (
            EventContentRaw::new(
                b"\xa2\x61m\x78\x18application/octet-stream\x61d\x81\x18\x00".to_vec(),
            ),
            false,
        ),
        (
            EventContentRaw::new([body(PUBLIC_ATTACHMENT_MIME, &[1]).as_ref(), &[0]].concat()),
            false,
        ),
    ] {
        let db = Database::new_in_memory(secret.id()).await?;
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain_indexed(&db, event, raw).await?;
        let page = db.list_own_public_attachments(None).await?;
        assert_progress(&page);
        assert_eq!(page.items.len(), usize::from(accepted));
        assert_eq!(page.skipped_rows, usize::from(!accepted));
        if let Some(item) = page.items.first() {
            let OpaqueMediaRead::Available { data, .. } = db
                .read_native_media_opaque(secret.id(), MediaEventReference::Full(item.event_id))
                .await?
            else {
                panic!("candidate failed unchanged exact read");
            };
            assert_eq!(item.bytes, data.len());
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachments_do_not_trust_index_or_corrupt_retained_storage() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    for corruption in 0..10 {
        let db = Database::new_in_memory(secret.id()).await?;
        let raw = body(PUBLIC_ATTACHMENT_MIME, &[1]);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain_indexed(&db, event, raw.clone()).await?;
        db.write_with(|tx| {
            let key =
                bincode::encode_to_vec(event.compute_short_id(), redb_bincode::BINCODE_CONFIG)
                    .unwrap();
            let hash =
                bincode::encode_to_vec(event.content_hash(), redb_bincode::BINCODE_CONFIG).unwrap();
            match corruption {
                0 => {
                    tx.open_table(&crate::events::TABLE)?
                        .remove(&event.compute_short_id())?;
                }
                1 | 2 => {
                    let other =
                        signed(RostraIdSecretKey::generate(), &raw, EventKind::SOCIAL_MEDIA);
                    let replacement = if corruption == 1 {
                        other
                    } else {
                        SignedEvent::unverified(*event.event(), other.sig())
                    };
                    tx.open_table(&crate::events::TABLE)?.insert(
                        &event.compute_short_id(),
                        &EventRecord {
                            signed: replacement,
                        },
                    )?;
                }
                3 => {
                    tx.open_table(&crate::content_store::TABLE)?
                        .remove(&event.content_hash())?;
                }
                4..=6 => {
                    let mut store = tx
                        .as_raw()
                        .open_table(crate::content_store::TABLE.as_raw())?;
                    let bytes = match corruption {
                        4 => vec![255; 9], // forged enormous borrowed length
                        5 => vec![0; MAX_NATIVE_MEDIA_BODY_BYTES + 10],
                        _ => bincode::encode_to_vec(
                            ContentStoreRecord(Cow::Owned(body(PUBLIC_ATTACHMENT_MIME, &[2]))),
                            redb_bincode::BINCODE_CONFIG,
                        )
                        .unwrap(),
                    };
                    store.insert(hash.as_slice(), bytes.as_slice())?;
                }
                7 => {
                    tx.as_raw()
                        .open_table(crate::events::TABLE.as_raw())?
                        .insert(key.as_slice(), &[0xff][..])?;
                }
                8 => {
                    tx.as_raw()
                        .open_table(crate::events_content_state::TABLE.as_raw())?
                        .insert(key.as_slice(), &[0xff][..])?;
                }
                9 => {
                    // A foreign signed event under its own correct short key is
                    // still not ours just because it appears in the self index.
                    let other =
                        signed(RostraIdSecretKey::generate(), &raw, EventKind::SOCIAL_MEDIA);
                    tx.open_table(&crate::events_self::TABLE)?
                        .remove(&event.compute_short_id())?;
                    tx.open_table(&crate::events_self::TABLE)?
                        .insert(&other.compute_short_id(), &())?;
                    tx.open_table(&crate::events::TABLE)?
                        .insert(&other.compute_short_id(), &EventRecord { signed: other })?;
                }
                _ => unreachable!(),
            }
            Ok(())
        })
        .await?;
        let page = db.list_own_public_attachments(None).await?;
        assert_progress(&page);
        assert!(page.items.is_empty(), "corruption {corruption}");
        assert_eq!(page.skipped_rows, 1);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachments_malformed_index_fails_instead_of_panicking_or_looping()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    for (key, value) in [
        (vec![0xff], vec![]),
        (vec![0; 17], vec![]),
        (vec![0; 16], vec![1]),
    ] {
        let db = Database::new_in_memory(secret.id()).await?;
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::events_self::TABLE.as_raw())?
                .insert(key.as_slice(), value.as_slice())?;
            Ok(())
        })
        .await?;
        assert!(matches!(
            db.list_own_public_attachments(None).await,
            Err(DbError::MediaCandidate)
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn public_attachments_exact_budget_requires_explicit_exhaustion() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for n in 0..8 {
        let raw = body(PUBLIC_ATTACHMENT_MIME, &[n]);
        let event = signed(secret, &raw, EventKind::SOCIAL_MEDIA);
        retain_indexed(&db, event, raw).await?;
    }
    let page = db.list_own_public_attachments(None).await?;
    assert_progress(&page);
    assert_eq!(page.items.len(), 8);
    assert_eq!(page.stop, OwnPublicAttachmentStop::BudgetReached);
    let end = db
        .list_own_public_attachments(page.next_cursor.as_ref())
        .await?;
    assert_progress(&end);
    assert_eq!(end.raw_rows_scanned, 0);
    assert_eq!(end.stop, OwnPublicAttachmentStop::Exhausted);
    Ok(())
}
