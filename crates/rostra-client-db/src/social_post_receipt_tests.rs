use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;

use crate::social::ReceivedAtPaginationCursor;
use crate::{
    Database, DbError, db_version, events, events_content_state, reception_order_next,
    social_posts_by_received_at, social_posts_by_time, social_posts_received_at_keys,
};

fn social_post(secret: RostraIdSecretKey, text: &str, event_ts: i64) -> VerifiedEventContent {
    let content = content_kind::SocialPost::new_text(text.to_owned(), None, Default::default())
        .serialize_cbor()
        .expect("social post must serialize");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(event_ts).expect("valid timestamp"))
        .content(&content)
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify");
    VerifiedEventContent::assume_verified(event, content)
}

fn deletion(
    secret: RostraIdSecretKey,
    target: &VerifiedEventContent,
    event_ts: i64,
) -> VerifiedEvent {
    let content = EventContentRaw::new(vec![]);
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(event_ts).expect("valid timestamp"))
        .parent_prev(target.event_id().into())
        .delete(target.event_id().into())
        .content(&content)
        .build()
        .signed_by(secret);
    VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify")
}

async fn process_at(
    db: &Database,
    content: &VerifiedEventContent,
    received_at: Timestamp,
) -> BoxedErrorResult<()> {
    db.write_with(|tx| {
        db.process_event_tx(&content.event, received_at, tx)?;
        db.process_event_content_tx(content, received_at, tx)
    })
    .await
    .boxed()?;
    Ok(())
}

async fn receipt_rows(
    db: &Database,
) -> BoxedErrorResult<(
    Vec<((Timestamp, u64), ShortEventId)>,
    Vec<(ShortEventId, (Timestamp, u64))>,
    Option<u64>,
)> {
    Ok(db
        .read_with(|tx| {
            let forward = tx
                .open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, event_id)| (key.value(), event_id.value())))
                .collect::<Result<Vec<_>, _>>()?;
            let mut reverse = tx
                .open_table(&social_posts_received_at_keys::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(event_id, key)| (event_id.value(), key.value())))
                .collect::<Result<Vec<_>, _>>()?;
            reverse.sort_unstable_by_key(|(_, key)| *key);
            let next = tx
                .open_table(&reception_order_next::TABLE)?
                .get(&())?
                .map(|entry| entry.value());
            Ok((forward, reverse, next))
        })
        .await?)
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn social_receipt_reversion_updates_cursors_without_reusing_order() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([87; 32]);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let old = social_post(secret, "old", 100);
    let removed = social_post(secret, "removed", 101);
    let later = social_post(secret, "later", 102);
    let old_id = old.event_id().to_short();
    let removed_id = removed.event_id().to_short();
    let later_id = later.event_id().to_short();

    {
        let db = Database::open(&path, secret.id()).await.boxed()?;
        process_at(&db, &old, Timestamp::from(700)).await?;
        process_at(&db, &removed, Timestamp::from(900)).await?;
        assert_eq!(
            receipt_rows(&db).await?,
            (
                vec![
                    ((Timestamp::from(700), 1), old_id),
                    ((Timestamp::from(900), 3), removed_id),
                ],
                vec![
                    (old_id, (Timestamp::from(700), 1)),
                    (removed_id, (Timestamp::from(900), 3)),
                ],
                Some(4),
            )
        );

        let deleting = deletion(secret, &removed, 103);
        db.write_with(|tx| db.process_event_tx(&deleting, Timestamp::from(950), tx))
            .await?;
        db.write_with(|tx| db.process_event_tx(&deleting, Timestamp::from(950), tx))
            .await?;
        assert_eq!(
            receipt_rows(&db).await?,
            (
                vec![((Timestamp::from(700), 1), old_id)],
                vec![(old_id, (Timestamp::from(700), 1))],
                Some(5),
            ),
            "deletion must remove both receipt directions without reclaiming order"
        );
        assert_eq!(
            db.get_latest_social_post_received_at_cursor().await,
            Some(ReceivedAtPaginationCursor {
                ts: Timestamp::from(700),
                seq: 1,
            })
        );
    }

    let db = Database::open(&path, secret.id()).await.boxed()?;
    process_at(&db, &later, Timestamp::from(900)).await?;
    assert_eq!(
        receipt_rows(&db).await?,
        (
            vec![
                ((Timestamp::from(700), 1), old_id),
                ((Timestamp::from(900), 6), later_id),
            ],
            vec![
                (old_id, (Timestamp::from(700), 1)),
                (later_id, (Timestamp::from(900), 6)),
            ],
            Some(7),
        ),
        "reopen must preserve allocator durability and collision safety"
    );
    assert_eq!(
        db.get_latest_social_post_received_at_cursor().await,
        Some(ReceivedAtPaginationCursor {
            ts: Timestamp::from(900),
            seq: 6,
        })
    );
    let (forward, forward_cursor) = db
        .paginate_social_posts_by_received_at(None, 1, |_| true)
        .await;
    assert_eq!(
        forward.iter().map(|post| post.event_id).collect::<Vec<_>>(),
        vec![old_id]
    );
    assert_eq!(
        forward_cursor,
        Some(ReceivedAtPaginationCursor {
            ts: Timestamp::from(900),
            seq: 6,
        }),
        "forward continuation must point to the retained later post"
    );
    let (reverse, reverse_cursor) = db
        .paginate_social_posts_by_received_at_rev(None, 1, |_| true)
        .await;
    assert_eq!(
        reverse.iter().map(|post| post.event_id).collect::<Vec<_>>(),
        vec![later_id]
    );
    assert_eq!(
        reverse_cursor,
        Some(ReceivedAtPaginationCursor {
            ts: Timestamp::from(700),
            seq: 1,
        }),
        "reverse continuation must point to the retained older post"
    );

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn inconsistent_social_receipt_mapping_aborts_complete_deletion() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([88; 32]);
    let target = social_post(secret, "target", 200);
    let target_id = target.event_id().to_short();

    for actual_event_id in [None, Some(ShortEventId::ZERO)] {
        let db = Database::new_in_memory(secret.id()).await?;
        process_at(&db, &target, Timestamp::from(800)).await?;
        let before = receipt_rows(&db).await?;
        let receipt_key = before.1[0].1;

        db.write_with(|tx| {
            let mut receipts = tx.open_table(&social_posts_by_received_at::TABLE)?;
            if let Some(actual_event_id) = actual_event_id {
                receipts.insert(&receipt_key, &actual_event_id)?;
            } else {
                receipts.remove(&receipt_key)?;
            }
            Ok(())
        })
        .await?;
        let corrupted = receipt_rows(&db).await?;
        let deleting = deletion(secret, &target, 201);
        let error = db
            .write_with(|tx| db.process_event_tx(&deleting, Timestamp::from(900), tx))
            .await
            .expect_err("mismatched receipt mapping must fail deletion");
        let DbError::SocialPostReceiptMismatch {
            event_id,
            actual_event_id: observed_actual_event_id,
            ..
        } = error
        else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(event_id, target_id);
        assert_eq!(observed_actual_event_id, actual_event_id);
        assert_eq!(receipt_rows(&db).await?, corrupted);
        db.read_with(|tx| {
            assert!(
                tx.open_table(&events::TABLE)?
                    .get(&deleting.event_id.to_short())?
                    .is_none()
            );
            assert!(
                tx.open_table(&events_content_state::TABLE)?
                    .get(&target_id)?
                    .is_none(),
                "target must remain Processed after rollback"
            );
            Ok(())
        })
        .await?;
    }

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn duplicate_social_receipt_mapping_aborts_complete_insertion() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([90; 32]);
    let db = Database::new_in_memory(secret.id()).await?;
    let target = social_post(secret, "target", 250);
    let target_id = target.event_id().to_short();
    let seeded_key = (Timestamp::from(600), 40);

    db.write_with(|tx| {
        tx.open_table(&social_posts_received_at_keys::TABLE)?
            .insert(&target_id, &seeded_key)?;
        Ok(())
    })
    .await?;
    let error = db
        .try_process_event_with_content(&target)
        .await
        .expect_err("duplicate reverse mapping must fail insertion");
    assert!(matches!(
        error,
        DbError::SocialPostReceiptAlreadyIndexed { event_id, .. }
            if event_id == target_id
    ));
    assert_eq!(
        receipt_rows(&db).await?,
        (vec![], vec![(target_id, seeded_key)], None),
        "failed insertion must preserve the mapping and allocator"
    );
    db.read_with(|tx| {
        assert!(tx.open_table(&events::TABLE)?.get(&target_id)?.is_none());
        assert!(
            tx.open_table(&social_posts_by_time::TABLE)?
                .get(&(target.timestamp(), target_id))?
                .is_none(),
            "failed insertion must roll back earlier projection writes"
        );
        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn version_24_unmapped_receipt_is_rebuilt_before_open() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([89; 32]);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let legacy = social_post(secret, "legacy", 300);
    let legacy_id = legacy.event_id().to_short();

    {
        let db = Database::open(&path, secret.id()).await.boxed()?;
        process_at(&db, &legacy, Timestamp::from(700)).await?;
    }
    {
        let db = redb_bincode::Database::from(redb::Database::open(&path)?);
        let tx = db.begin_write()?;
        assert!(
            tx.as_raw()
                .delete_table(social_posts_received_at_keys::TABLE.as_raw())?
        );
        tx.open_table(&db_version::TABLE)?.insert(&(), &24)?;
        tx.commit()?;
    }

    let replayed = Database::open(&path, secret.id()).await.boxed()?;
    assert_eq!(
        receipt_rows(&replayed).await?,
        (
            vec![((Timestamp::from(300), 1), legacy_id)],
            vec![(legacy_id, (Timestamp::from(300), 1))],
            Some(2),
        ),
        "version-24 replay must rebuild authored-time receipt membership in both directions"
    );
    assert_eq!(
        replayed
            .read_with(|tx| {
                Ok(tx
                    .open_table(&db_version::TABLE)?
                    .get(&())?
                    .map(|entry| entry.value()))
            })
            .await?,
        Some(26)
    );

    let delete_legacy = deletion(secret, &legacy, 302);
    replayed
        .write_with(|tx| replayed.process_event_tx(&delete_legacy, Timestamp::from(800), tx))
        .await?;
    assert_eq!(
        receipt_rows(&replayed).await?,
        (vec![], vec![], Some(3)),
        "rebuilt exact mapping must support bounded reversion"
    );

    Ok(())
}
