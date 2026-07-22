use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{Event, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;

use crate::{
    Database, DbError, EventReceivedRecord, EventReceivedSource, OverflowSnafu, content_store,
    events, events_content_state, events_received_at, reception_order_next,
    shoutbox_posts_by_received_at, social_posts_by_received_at,
};

fn social_post(secret: RostraIdSecretKey, text: &str, event_ts: i64) -> VerifiedEventContent {
    let content =
        content_kind::SocialPost::new(text.to_owned(), None, Default::default()).serialize_cbor();
    content_event(secret, EventKind::SOCIAL_POST, content.unwrap(), event_ts)
}

fn shoutbox_post(secret: RostraIdSecretKey, text: &str, event_ts: i64) -> VerifiedEventContent {
    let content = content_kind::Shoutbox {
        djot_content: text.to_owned(),
    }
    .serialize_cbor();
    content_event(secret, EventKind::SHOUTBOX, content.unwrap(), event_ts)
}

fn content_event(
    secret: RostraIdSecretKey,
    kind: EventKind,
    content: rostra_core::event::EventContentRaw,
    event_ts: i64,
) -> VerifiedEventContent {
    let author = secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(kind)
        .content(&content)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(event_ts).unwrap())
        .build();
    let event = VerifiedEvent::verify_signed(author, event.signed_by(secret)).unwrap();
    VerifiedEventContent::assume_verified(event, content)
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

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn reception_order_survives_reopen_for_all_indexes() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([31; 32]);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let received_at = Timestamp::from(700);
    let social_a = social_post(secret, "social-a", 101);
    let shout_a = shoutbox_post(secret, "shout-a", 102);
    let social_b = social_post(secret, "social-b", 103);
    let shout_b = shoutbox_post(secret, "shout-b", 104);

    {
        let db = Database::open(&path, secret.id()).await.boxed()?;
        process_at(&db, &social_a, received_at).await?;
        process_at(&db, &shout_a, received_at).await?;
    }

    let db = Database::open(&path, secret.id()).await.boxed()?;
    process_at(&db, &social_b, received_at).await?;
    process_at(&db, &shout_b, received_at).await?;

    db.read_with(|tx| {
        let event_receipts = tx
            .open_table(&events_received_at::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, value)| (key.value(), value.value().event_id)))
            .collect::<Result<Vec<_>, _>>()?;
        let social_receipts = tx
            .open_table(&social_posts_by_received_at::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, value)| (key.value(), value.value())))
            .collect::<Result<Vec<_>, _>>()?;
        let shout_receipts = tx
            .open_table(&shoutbox_posts_by_received_at::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, value)| (key.value(), value.value())))
            .collect::<Result<Vec<_>, _>>()?;
        let next = tx
            .open_table(&reception_order_next::TABLE)?
            .get(&())?
            .map(|value| value.value());

        assert_eq!(
            event_receipts
                .iter()
                .map(|entry| entry.0)
                .collect::<Vec<_>>(),
            vec![
                (received_at, 0),
                (received_at, 2),
                (received_at, 4),
                (received_at, 6),
            ]
        );
        assert_eq!(
            social_receipts,
            vec![
                (received_at, 1, social_a.event_id().to_short()),
                (received_at, 5, social_b.event_id().to_short()),
            ]
            .into_iter()
            .map(|(ts, seq, id)| ((ts, seq), id))
            .collect::<Vec<_>>()
        );
        assert_eq!(
            shout_receipts,
            vec![
                (received_at, 3, shout_a.event_id().to_short()),
                (received_at, 7, shout_b.event_id().to_short()),
            ]
            .into_iter()
            .map(|(ts, seq, id)| ((ts, seq), id))
            .collect::<Vec<_>>()
        );
        assert_eq!(next, Some(8));
        Ok(())
    })
    .await?;

    let (social, _) = db
        .paginate_social_posts_by_received_at(None, 10, |_| true)
        .await;
    assert_eq!(
        social.iter().map(|post| post.event_id).collect::<Vec<_>>(),
        vec![
            social_a.event_id().to_short(),
            social_b.event_id().to_short()
        ]
    );
    let (shoutbox, _) = db
        .paginate_shoutbox_posts_by_received_at_rev(None, 10)
        .await;
    assert_eq!(
        shoutbox
            .iter()
            .map(|post| post.event_id)
            .collect::<Vec<_>>(),
        vec![shout_b.event_id().to_short(), shout_a.event_id().to_short()]
    );

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn reception_order_allocation_is_transactional_and_checked() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([32; 32]);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let db = Database::open(&path, secret.id()).await.boxed()?;
    let received_at = Timestamp::from(900);
    let first = EventReceivedRecord {
        event_id: ShortEventId::ZERO,
        source: EventReceivedSource::Local,
    };
    let second = EventReceivedRecord {
        event_id: ShortEventId::MAX,
        source: EventReceivedSource::Local,
    };

    let allocated = db
        .write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            Ok([
                Database::insert_reception_ordered_tx(tx, received_at, &first, &mut table)?,
                Database::insert_reception_ordered_tx(tx, received_at, &second, &mut table)?,
            ])
        })
        .await?;
    assert_eq!(allocated, [0, 1]);

    let aborted = db
        .write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            assert_eq!(
                Database::insert_reception_ordered_tx(tx, received_at, &first, &mut table)?,
                2
            );
            OverflowSnafu.fail::<()>()
        })
        .await;
    assert!(matches!(aborted, Err(DbError::Overflow)));
    assert_eq!(
        db.write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            Database::insert_reception_ordered_tx(tx, received_at, &first, &mut table)
        })
        .await?,
        2,
        "an aborted allocation must remain available"
    );
    drop(db);

    let db = Database::open(&path, secret.id()).await.boxed()?;
    assert_eq!(
        db.write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            Database::insert_reception_ordered_tx(tx, received_at, &second, &mut table)
        })
        .await?,
        3
    );

    db.write_with(|tx| {
        tx.open_table(&reception_order_next::TABLE)?
            .insert(&(), &(u64::MAX - 1))?;
        Ok(())
    })
    .await?;
    let overflowed = db
        .write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            assert_eq!(
                Database::insert_reception_ordered_tx(tx, received_at, &first, &mut table)?,
                u64::MAX - 1
            );
            Database::insert_reception_ordered_tx(tx, received_at, &second, &mut table)?;
            Ok(())
        })
        .await;
    assert!(matches!(overflowed, Err(DbError::Overflow)));
    assert_eq!(
        db.write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            Database::insert_reception_ordered_tx(tx, received_at, &first, &mut table)
        })
        .await?,
        u64::MAX - 1,
        "overflow must roll back earlier allocations in the transaction"
    );
    assert!(matches!(
        db.write_with(|tx| {
            let mut table = tx.open_table(&events_received_at::TABLE)?;
            Database::insert_reception_ordered_tx(tx, received_at, &second, &mut table)
        })
        .await,
        Err(DbError::Overflow)
    ));

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn reception_index_collisions_never_replace_members() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([33; 32]);
    let (_dir, db) = crate::tests::temp_db(secret.id()).await?;
    let received_at = Timestamp::from(800);
    let existing_id = ShortEventId::ZERO;

    let event = social_post(secret, "event collision", 201);
    db.write_with(|tx| {
        tx.open_table(&events_received_at::TABLE)?.insert(
            &(received_at, 0),
            &EventReceivedRecord {
                event_id: existing_id,
                source: EventReceivedSource::Local,
            },
        )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.write_with(|tx| db.process_event_tx(&event.event, received_at, tx))
            .await,
        Err(DbError::ReceptionOrderCollision {
            ref table,
            ..
        }) if table == "events_received_at"
    ));
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&events_received_at::TABLE)?
                .get(&(received_at, 0))?
                .map(|value| value.value().event_id),
            Some(existing_id)
        );
        assert!(
            tx.open_table(&events::TABLE)?
                .get(&event.event_id().to_short())?
                .is_none()
        );
        Ok(())
    })
    .await?;

    let social = social_post(secret, "social collision", 202);
    db.write_with(|tx| {
        db.process_event_tx(&social.event, Timestamp::from(801), tx)?;
        tx.open_table(&reception_order_next::TABLE)?
            .insert(&(), &20)?;
        tx.open_table(&social_posts_by_received_at::TABLE)?
            .insert(&(received_at, 20), &existing_id)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.write_with(|tx| db.process_event_content_tx(&social, received_at, tx))
            .await,
        Err(DbError::ReceptionOrderCollision {
            ref table,
            ..
        }) if table == "social_posts_by_received_at"
    ));
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&social_posts_by_received_at::TABLE)?
                .get(&(received_at, 20))?
                .map(|value| value.value()),
            Some(existing_id)
        );
        assert!(
            tx.open_table(&events_content_state::TABLE)?
                .get(&social.event_id().to_short())?
                .is_some()
        );
        assert!(
            tx.open_table(&content_store::TABLE)?
                .get(&social.content_hash())?
                .is_none()
        );
        Ok(())
    })
    .await?;

    let shoutbox = shoutbox_post(secret, "shoutbox collision", 203);
    db.write_with(|tx| {
        db.process_event_tx(&shoutbox.event, Timestamp::from(802), tx)?;
        tx.open_table(&reception_order_next::TABLE)?
            .insert(&(), &30)?;
        tx.open_table(&shoutbox_posts_by_received_at::TABLE)?
            .insert(&(received_at, 30), &existing_id)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.write_with(|tx| db.process_event_content_tx(&shoutbox, received_at, tx))
            .await,
        Err(DbError::ReceptionOrderCollision {
            ref table,
            ..
        }) if table == "shoutbox_posts_by_received_at"
    ));
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&shoutbox_posts_by_received_at::TABLE)?
                .get(&(received_at, 30))?
                .map(|value| value.value()),
            Some(existing_id)
        );
        assert!(
            tx.open_table(&events_content_state::TABLE)?
                .get(&shoutbox.event_id().to_short())?
                .is_some()
        );
        Ok(())
    })
    .await?;

    Ok(())
}
