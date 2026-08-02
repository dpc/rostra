use std::num::NonZeroUsize;
use std::sync::Arc;

use redb::ReadableTable as _;
use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};

use crate::{
    Database, DbError, EventContentState, SocialPostMaterialization,
    SocialPostMaterializationCursor, content_store, events_content_state,
    social_post_materializations,
};

fn social_post(
    secret: RostraIdSecretKey,
    timestamp: i64,
    parent: Option<rostra_core::EventId>,
    replaced: Option<rostra_core::EventId>,
    body: &str,
) -> VerifiedEventContent {
    let content = content_kind::SocialPost::new(body.to_owned(), None, Default::default())
        .serialize_cbor()
        .expect("social post must serialize");
    verified_content(
        secret,
        EventKind::SOCIAL_POST,
        timestamp,
        parent,
        replaced,
        content,
    )
}

fn verified_content(
    secret: RostraIdSecretKey,
    kind: EventKind,
    timestamp: i64,
    parent: Option<rostra_core::EventId>,
    deleted: Option<rostra_core::EventId>,
    content: EventContentRaw,
) -> VerifiedEventContent {
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(kind)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .content(&content)
        .maybe_parent_prev(parent.map(Into::into))
        .maybe_delete(deleted.map(Into::into))
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify");
    VerifiedEventContent::assume_verified(event, content)
}

fn deletion(
    secret: RostraIdSecretKey,
    timestamp: i64,
    parent: rostra_core::EventId,
    target: rostra_core::EventId,
) -> VerifiedEvent {
    let content = EventContentRaw::new(vec![]);
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .parent_prev(parent.into())
        .delete(target.into())
        .content(&content)
        .build()
        .signed_by(secret);
    VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify")
}

async fn scan(
    db: &Database,
    after: Option<SocialPostMaterializationCursor>,
    limit: usize,
) -> crate::DbResult<crate::SocialPostMaterializationPage> {
    db.scan_social_post_materializations(
        after,
        NonZeroUsize::new(limit).expect("positive test limit"),
    )
    .await
}

async fn feed_rows(db: &Database) -> crate::DbResult<Vec<(u64, ShortEventId)>> {
    db.read_with(|tx| {
        tx.open_table(&social_post_materializations::TABLE)?
            .range::<u64>(..)?
            .map(|entry| {
                entry
                    .map(|(sequence, event_id)| (sequence.value(), event_id.value()))
                    .map_err(Into::into)
            })
            .collect()
    })
    .await
}

async fn replaced_pair(
    secret: RostraIdSecretKey,
    timestamp: i64,
) -> anyhow::Result<(Database, VerifiedEventContent)> {
    let db = Database::new_in_memory(secret.id()).await?;
    let original = social_post(secret, timestamp, None, None, "original");
    db.try_process_event_with_content(&original).await?;
    let edit = social_post(
        secret,
        timestamp + 1,
        Some(original.event_id()),
        Some(original.event_id()),
        "edit",
    );
    db.try_process_event_with_content(&edit).await?;
    Ok((db, original))
}

fn assert_present(item: &SocialPostMaterialization, expected: ExternalEventId, body: &str) {
    assert!(matches!(
        item,
        SocialPostMaterialization::Present {
            post_id,
            content,
            ..
        } if *post_id == expected && content.djot_content.as_deref() == Some(body)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn late_materialization_appends_once_and_excludes_nonordinary_paths() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 10, None, None, "late");

    db.try_process_event(&post.event).await?;
    assert!(scan(&db, None, 10).await?.items.is_empty());
    db.try_process_event_content(&post).await?;
    db.try_process_event_content(&post).await?;

    let malformed = verified_content(
        secret,
        EventKind::SOCIAL_POST,
        11,
        Some(post.event_id()),
        None,
        EventContentRaw::new(vec![0xff]),
    );
    db.try_process_event_with_content(&malformed).await?;
    let blank_delete = social_post(
        secret,
        12,
        Some(malformed.event_id()),
        Some(malformed.event_id()),
        "",
    );
    db.try_process_event_with_content(&blank_delete).await?;

    let predeleted = social_post(secret, 14, None, None, "already deleted");
    db.try_process_event(&deletion(
        secret,
        13,
        predeleted.event_id(),
        predeleted.event_id(),
    ))
    .await?;
    db.try_process_event_with_content(&predeleted).await?;

    let oversized = verified_content(
        secret,
        EventKind::SOCIAL_POST,
        15,
        Some(blank_delete.event_id()),
        None,
        EventContentRaw::new(vec![0; 10_000_000]),
    );
    db.try_process_event_with_content(&oversized).await?;

    let page = scan(&db, None, 10).await?;
    assert_eq!(page.items.len(), 1);
    assert_present(
        &page.items[0],
        ExternalEventId::new(secret.id(), post.event_id().to_short()),
        "late",
    );
    assert!(page.exhausted);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn deletion_and_replacement_resolve_old_occurrences_as_removed() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let original = social_post(secret, 20, None, None, "original");
    db.try_process_event_with_content(&original).await?;
    let edit = social_post(
        secret,
        21,
        Some(original.event_id()),
        Some(original.event_id()),
        "edit",
    );
    db.try_process_event_with_content(&edit).await?;

    let page = scan(&db, None, 10).await?;
    assert_eq!(page.items.len(), 2);
    assert!(matches!(
        page.items[0],
        SocialPostMaterialization::Removed { post_id }
            if post_id == ExternalEventId::new(secret.id(), original.event_id().to_short())
    ));
    assert_present(
        &page.items[1],
        ExternalEventId::new(secret.id(), edit.event_id().to_short()),
        "edit",
    );

    db.try_process_event(&deletion(secret, 22, edit.event_id(), edit.event_id()))
        .await?;
    let page = scan(&db, None, 10).await?;
    assert!(
        page.items
            .iter()
            .all(|item| matches!(item, SocialPostMaterialization::Removed { .. }))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cursor_is_bounded_resumable_and_safe_for_checkpoint_replay() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    assert!(matches!(
        db.scan_social_post_materializations(None, NonZeroUsize::new(usize::MAX).unwrap())
            .await,
        Err(DbError::SocialPostMaterializationScanLimitTooHigh { .. })
    ));
    for (timestamp, body) in [(30, "a"), (31, "b"), (32, "c")] {
        db.try_process_event_with_content(&social_post(secret, timestamp, None, None, body))
            .await?;
    }

    let first = scan(&db, None, 2).await?;
    assert_eq!(first.items.len(), 2);
    assert!(!first.exhausted);
    let repeated = scan(&db, None, 2).await?;
    assert_eq!(
        repeated, first,
        "a crash before checkpoint repeats the page"
    );

    let last = scan(&db, Some(first.scanned_through), 2).await?;
    assert_eq!(last.items.len(), 1);
    assert!(last.exhausted);
    let empty = scan(&db, Some(last.scanned_through), 2).await?;
    assert!(empty.items.is_empty());
    assert!(empty.exhausted);
    assert_eq!(empty.scanned_through, last.scanned_through);

    let out_of_range: SocialPostMaterializationCursor = serde_json::from_str("99")?;
    assert!(matches!(
        scan(&db, Some(out_of_range), 1).await,
        Err(DbError::SocialPostMaterializationCursorOutOfRange { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn materialization_tip_handles_empty_and_nonempty_feeds() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;

    let empty_tip = db.get_social_post_materialization_tip().await?;
    assert_eq!(empty_tip, scan(&db, None, 1).await?.scanned_through);

    db.try_process_event_with_content(&social_post(secret, 35, None, None, "baseline"))
        .await?;
    let nonempty_tip = db.get_social_post_materialization_tip().await?;
    let at_tip = scan(&db, Some(nonempty_tip), 1).await?;
    assert!(at_tip.items.is_empty());
    assert!(at_tip.exhausted);

    let after_post = social_post(secret, 36, None, None, "after");
    db.try_process_event_with_content(&after_post).await?;
    let after = scan(&db, Some(nonempty_tip), 1).await?;
    assert_eq!(after.items.len(), 1);
    assert_present(
        &after.items[0],
        ExternalEventId::new(secret.id(), after_post.event_id().to_short()),
        "after",
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn materialization_tip_returns_corruption_and_overflow_errors() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(social_post_materializations::TABLE.as_raw())?
            .insert(&[0xff][..], &[][..])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.get_social_post_materialization_tip().await,
        Err(DbError::StoredDecode { .. })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    let max_key = bincode::encode_to_vec(u64::MAX, redb_bincode::BINCODE_CONFIG)?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(social_post_materializations::TABLE.as_raw())?
            .insert(max_key.as_slice(), &[][..])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.get_social_post_materialization_tip().await,
        Err(DbError::Overflow)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn materialization_tip_is_a_concurrent_enablement_boundary() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    db.try_process_event_with_content(&social_post(secret, 37, None, None, "baseline"))
        .await?;

    let start = Arc::new(tokio::sync::Barrier::new(2));
    let tip = {
        let db = Arc::clone(&db);
        let start = Arc::clone(&start);
        async move {
            start.wait().await;
            db.get_social_post_materialization_tip().await
        }
    };
    let raced_write = {
        let db = Arc::clone(&db);
        let start = Arc::clone(&start);
        async move {
            start.wait().await;
            db.try_process_event_with_content(&social_post(secret, 38, None, None, "raced"))
                .await
        }
    };
    let (tip, write_result) = tokio::join!(tip, raced_write);
    let tip = tip?;
    write_result?;

    db.try_process_event_with_content(&social_post(secret, 39, None, None, "after"))
        .await?;
    let page = scan(&db, Some(tip), 3).await?;
    let bodies = page
        .items
        .iter()
        .map(|item| match item {
            SocialPostMaterialization::Present { content, .. } => content
                .djot_content
                .as_deref()
                .expect("test post has a body"),
            SocialPostMaterialization::Removed { .. } => panic!("test post remains present"),
        })
        .collect::<Vec<_>>();
    assert!(
        bodies == ["after"] || bodies == ["raced", "after"],
        "the racing commit must fall entirely before or after the observed tip"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_enclosing_transaction_does_not_publish_feed_or_projection() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 40, None, None, "atomic");
    let result = db
        .write_with(|tx| {
            db.process_event_tx(&post.event, rostra_core::Timestamp::from(40), tx)?;
            db.process_event_content_tx(&post, rostra_core::Timestamp::from(40), tx)?;
            Err::<(), _>(DbError::Overflow)
        })
        .await;
    assert!(matches!(result, Err(DbError::Overflow)));
    assert!(scan(&db, None, 1).await?.items.is_empty());
    assert!(
        db.get_social_post(post.event_id().to_short())
            .await
            .is_none()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn sequence_exhaustion_rolls_back_materialization() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    db.write_with(|tx| {
        tx.open_table(&social_post_materializations::TABLE)?
            .insert(&(u64::MAX - 1), &ShortEventId::ZERO)?;
        Ok(())
    })
    .await?;
    let post = social_post(secret, 41, None, None, "overflow");

    assert!(matches!(
        db.try_process_event_with_content(&post).await,
        Err(DbError::Overflow)
    ));
    assert!(
        db.get_social_post(post.event_id().to_short())
            .await
            .is_none()
    );
    assert_eq!(
        db.read_with(|tx| {
            Ok(tx
                .open_table(&social_post_materializations::TABLE)?
                .range::<u64>(..)?
                .collect::<Result<Vec<_>, _>>()?
                .len())
        })
        .await?,
        1
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn corruption_returns_no_page_or_ack() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();

    let db = Database::new_in_memory(secret.id()).await?;
    db.write_with(|tx| {
        tx.open_table(&social_post_materializations::TABLE)?
            .insert(&1, &ShortEventId::ZERO)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::SocialPostMaterializationLogGap {
            expected: 0,
            actual: 1
        })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 39, None, None, "valid prefix");
    db.try_process_event_with_content(&post).await?;
    db.write_with(|tx| {
        tx.open_table(&social_post_materializations::TABLE)?
            .insert(&2, &ShortEventId::ZERO)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::SocialPostMaterializationLogGap {
            expected: 1,
            actual: 2
        })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    db.write_with(|tx| {
        tx.open_table(&social_post_materializations::TABLE)?
            .insert(&0, &ShortEventId::ZERO)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::MissingSocialPostMaterializationEvent { .. })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    let raw = verified_content(
        secret,
        EventKind::RAW,
        40,
        None,
        None,
        EventContentRaw::new(vec![]),
    );
    db.try_process_event_with_content(&raw).await?;
    db.write_with(|tx| {
        tx.open_table(&social_post_materializations::TABLE)?
            .insert(&0, &raw.event_id().to_short())?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::InvalidSocialPostMaterializationKind { .. })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 41, None, None, "state");
    db.try_process_event_with_content(&post).await?;
    db.write_with(|tx| {
        tx.open_table(&events_content_state::TABLE)?.insert(
            &post.event_id().to_short(),
            &EventContentState::Missing {
                last_fetch_attempt: None,
                fetch_attempt_count: 0,
                next_fetch_attempt: rostra_core::Timestamp::ZERO,
            },
        )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::MissingSocialPostMaterializationState { .. })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 42, None, None, "missing bytes");
    db.try_process_event_with_content(&post).await?;
    db.write_with(|tx| {
        tx.open_table(&content_store::TABLE)?
            .remove(&post.content_hash())?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::MissingSocialPostMaterializationContent { .. })
    ));

    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 43, None, None, "invalid bytes");
    db.try_process_event_with_content(&post).await?;
    db.write_with(|tx| {
        tx.open_table(&content_store::TABLE)?.insert(
            &post.content_hash(),
            &crate::event::ContentStoreRecord(std::borrow::Cow::Owned(EventContentRaw::new(vec![
                0xff,
            ]))),
        )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::InvalidSocialPostMaterializationContent { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_does_not_mask_processed_content_corruption() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();

    let (db, original) = replaced_pair(secret, 70).await?;
    db.write_with(|tx| {
        tx.open_table(&events_content_state::TABLE)?.insert(
            &original.event_id().to_short(),
            &EventContentState::Missing {
                last_fetch_attempt: None,
                fetch_attempt_count: 0,
                next_fetch_attempt: rostra_core::Timestamp::ZERO,
            },
        )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::MissingSocialPostMaterializationState { .. })
    ));

    let (db, original) = replaced_pair(secret, 72).await?;
    db.write_with(|tx| {
        tx.open_table(&events_content_state::TABLE)?
            .insert(&original.event_id().to_short(), &EventContentState::Invalid)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::InvalidSocialPostMaterializationState { .. })
    ));

    let (db, original) = replaced_pair(secret, 74).await?;
    db.write_with(|tx| {
        tx.open_table(&events_content_state::TABLE)?
            .remove(&original.event_id().to_short())?;
        tx.open_table(&content_store::TABLE)?
            .remove(&original.content_hash())?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::MissingSocialPostMaterializationContent { .. })
    ));

    let (db, original) = replaced_pair(secret, 76).await?;
    db.write_with(|tx| {
        tx.open_table(&events_content_state::TABLE)?
            .remove(&original.event_id().to_short())?;
        tx.open_table(&content_store::TABLE)?.insert(
            &original.content_hash(),
            &crate::event::ContentStoreRecord(std::borrow::Cow::Owned(EventContentRaw::new(vec![
                0xff,
            ]))),
        )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        scan(&db, None, 1).await,
        Err(DbError::InvalidSocialPostMaterializationContent { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_cutover_does_not_backfill_existing_posts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let secret = RostraIdSecretKey::generate();
    let post = social_post(secret, 50, None, None, "pre-cutover");
    let db = Database::open(&path, secret.id()).await?;
    db.try_process_event_with_content(&post).await?;
    drop(db);

    let raw = redb_bincode::Database::from(redb::Database::open(&path)?);
    let tx = raw.begin_write()?;
    tx.as_raw()
        .delete_table(social_post_materializations::TABLE.as_raw())?;
    tx.open_table(&crate::db_version::TABLE)?.insert(&(), &25)?;
    tx.commit()?;
    drop(raw);

    let db = Database::open(&path, secret.id()).await?;
    assert!(
        db.get_social_post(post.event_id().to_short())
            .await
            .is_some()
    );
    assert!(scan(&db, None, 10).await?.items.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn total_rebuild_preserves_feed_and_suppresses_replay() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let post = social_post(secret, 60, None, None, "preserved");
    db.try_process_event_with_content(&post).await?;
    let before = scan(&db, None, 10).await?;

    db.write_with(|tx| Database::prepare_total_migration(tx, 26))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;

    let after = scan(&db, None, 10).await?;
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_total_rebuild_keeps_feed_and_stash_retryable() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for (timestamp, body) in [(80, "first"), (81, "second")] {
        db.try_process_event_with_content(&social_post(secret, timestamp, None, None, body))
            .await?;
    }
    let feed_before = feed_rows(&db).await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 26))
        .await?;
    assert_eq!(feed_rows(&db).await?, feed_before);

    let events_temp = redb::TableDefinition::<&[u8], &[u8]>::new("_total_migration_events");
    let (saved_key, saved_value) = db
        .write_with(|tx| {
            let mut table = tx.as_raw().open_table(events_temp)?;
            let (key, value) = table.first()?.expect("migration has stashed events");
            let saved_key = key.value().to_vec();
            let saved_value = value.value().to_vec();
            drop(key);
            drop(value);
            table.insert(saved_key.as_slice(), &[0xff][..])?;
            Ok((saved_key, saved_value))
        })
        .await?;

    assert!(
        db.write_with(|tx| db.reprocess_migration_stash(tx))
            .await
            .is_err()
    );
    assert!(db.write_with(Database::has_pending_migration_stash).await?);
    assert_eq!(feed_rows(&db).await?, feed_before);

    db.write_with(|tx| {
        tx.as_raw()
            .open_table(events_temp)?
            .insert(saved_key.as_slice(), saved_value.as_slice())?;
        Ok(())
    })
    .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert_eq!(feed_rows(&db).await?, feed_before);
    assert_eq!(scan(&db, None, 10).await?.items.len(), 2);
    Ok(())
}
