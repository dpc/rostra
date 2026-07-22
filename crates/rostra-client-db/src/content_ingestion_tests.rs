use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ContentHash, EventId, ShortEventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tempfile::tempdir;

use crate::event::EventContentState;
use crate::{
    Database, IdsDataUsageRecord, content_rc, content_store, db_version, events,
    events_content_missing, events_content_state, events_received_at, ids_data_usage,
    social_posts_by_received_at, social_posts_by_time,
};

#[derive(Debug, PartialEq, Eq)]
struct IngestionSnapshot {
    event_present: bool,
    content: Option<Vec<u8>>,
    content_state: Option<EventContentState>,
    queue: Vec<(Timestamp, ShortEventId)>,
    content_rc: Option<u64>,
    usage: UsageSnapshot,
    event_receipts: Vec<ShortEventId>,
    posts_by_time: Vec<(Timestamp, ShortEventId)>,
    post_receipts: Vec<ShortEventId>,
}

#[derive(Debug, PartialEq, Eq)]
struct UsageSnapshot {
    current_metadata_size: u64,
    total_metadata_size: u64,
    current_metadata_num: u64,
    total_metadata_num: u64,
    current_content_size: u64,
    total_content_size: u64,
    current_payload_num: u64,
    total_payload_num: u64,
    missing_payload_size: u64,
    missing_payload_num: u64,
    deleted_payload_size: u64,
    deleted_payload_num: u64,
    pruned_payload_size: u64,
    pruned_payload_num: u64,
    invalid_payload_size: u64,
    invalid_payload_num: u64,
}

impl From<IdsDataUsageRecord> for UsageSnapshot {
    fn from(usage: IdsDataUsageRecord) -> Self {
        Self {
            current_metadata_size: usage.current_metadata_size,
            total_metadata_size: usage.total_metadata_size,
            current_metadata_num: usage.current_metadata_num,
            total_metadata_num: usage.total_metadata_num,
            current_content_size: usage.current_content_size,
            total_content_size: usage.total_content_size,
            current_payload_num: usage.current_payload_num,
            total_payload_num: usage.total_payload_num,
            missing_payload_size: usage.missing_payload_size,
            missing_payload_num: usage.missing_payload_num,
            deleted_payload_size: usage.deleted_payload_size,
            deleted_payload_num: usage.deleted_payload_num,
            pruned_payload_size: usage.pruned_payload_size,
            pruned_payload_num: usage.pruned_payload_num,
            invalid_payload_size: usage.invalid_payload_size,
            invalid_payload_num: usage.invalid_payload_num,
        }
    }
}

async fn snapshot(
    db: &Database,
    author: RostraId,
    event_id: ShortEventId,
    content_hash: ContentHash,
) -> BoxedErrorResult<IngestionSnapshot> {
    let content = db
        .get_event_content(event_id)
        .await
        .map(|content| content.as_ref().to_vec());
    Ok(db
        .read_with(|tx| {
            let event_present = tx.open_table(&events::TABLE)?.get(&event_id)?.is_some();
            let content_state = tx
                .open_table(&events_content_state::TABLE)?
                .get(&event_id)?
                .map(|entry| entry.value());
            let queue = tx
                .open_table(&events_content_missing::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value()))
                .collect::<Result<Vec<_>, _>>()?;
            let content_rc = tx
                .open_table(&content_rc::TABLE)?
                .get(&content_hash)?
                .map(|entry| entry.value());
            let usage =
                Database::get_data_usage_tx(author, &tx.open_table(&ids_data_usage::TABLE)?)?;
            let event_receipts = tx
                .open_table(&events_received_at::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(_, value)| value.value().event_id))
                .collect::<Result<Vec<_>, _>>()?;
            let posts_by_time = tx
                .open_table(&social_posts_by_time::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value()))
                .collect::<Result<Vec<_>, _>>()?;
            let post_receipts = tx
                .open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(_, value)| value.value()))
                .collect::<Result<Vec<_>, _>>()?;

            Ok(IngestionSnapshot {
                event_present,
                content,
                content_state,
                queue,
                content_rc,
                usage: usage.into(),
                event_receipts,
                posts_by_time,
                post_receipts,
            })
        })
        .await?)
}

fn post(secret: RostraIdSecretKey, text: &str) -> VerifiedEventContent {
    let content = content_kind::SocialPost::new(text.to_owned(), None, Default::default())
        .serialize_cbor()
        .expect("social post must serialize");
    verified_content(
        secret,
        EventKind::SOCIAL_POST,
        content,
        Some(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(42)),
    )
}

fn verified_content(
    secret: RostraIdSecretKey,
    kind: EventKind,
    content: EventContentRaw,
    timestamp: Option<time::OffsetDateTime>,
) -> VerifiedEventContent {
    let event = match timestamp {
        Some(timestamp) => Event::builder_raw_content()
            .author(secret.id())
            .kind(kind)
            .content(&content)
            .timestamp(timestamp)
            .build(),
        None => Event::builder_raw_content()
            .author(secret.id())
            .kind(kind)
            .content(&content)
            .build(),
    };
    let signed = event.signed_by(secret);
    let event =
        VerifiedEvent::verify_signed(secret.id(), signed).expect("event signature must verify");
    VerifiedEventContent::assume_verified(event, content)
}

fn deletion(secret: RostraIdSecretKey, parent: EventId, target: EventId) -> VerifiedEvent {
    let empty = EventContentRaw::new(vec![]);
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .parent_prev(parent.into())
        .delete(target.into())
        .content(&empty)
        .build()
        .signed_by(secret);
    VerifiedEvent::verify_signed(secret.id(), signed).expect("deletion signature must verify")
}

fn force_total_replay(path: &std::path::Path) -> BoxedErrorResult<()> {
    let db = redb_bincode::Database::from(redb::Database::open(path).boxed()?);
    let tx = db.begin_write().boxed()?;
    tx.open_table(&db_version::TABLE)?.insert(&(), &23)?;
    tx.commit().boxed()?;
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn public_content_ingestion_matches_combined_for_both_arrival_orders() -> BoxedErrorResult<()>
{
    let secret = RostraIdSecretKey::from_bytes([91; 32]);
    let author = secret.id();
    let content = post(secret, "content boundary");
    let event_id = content.event_id().to_short();
    let content_hash = content.content_hash();
    let mut snapshots = Vec::new();

    for scenario in ["combined", "content-first", "envelope-first"] {
        let dir = tempdir()?;
        let path = dir.path().join("db.redb");
        let db = Database::open(&path, author).await.boxed()?;

        match scenario {
            "combined" => {
                db.process_event_with_content(&content).await;
            }
            "content-first" => {
                db.process_event_content(&content).await;
                let before_late_envelope = snapshot(&db, author, event_id, content_hash).await?;
                assert!(before_late_envelope.event_present);
                assert_eq!(before_late_envelope.content_state, None);
                assert_eq!(before_late_envelope.content_rc, Some(1));
                assert!(before_late_envelope.queue.is_empty());
                assert_eq!(before_late_envelope.posts_by_time.len(), 1);
                db.process_event(&content.event).await;
                assert_eq!(
                    snapshot(&db, author, event_id, content_hash).await?,
                    before_late_envelope,
                    "late duplicate envelope changed content-first ingestion"
                );
            }
            "envelope-first" => {
                db.process_event(&content.event).await;
                let before_content = snapshot(&db, author, event_id, content_hash).await?;
                assert!(matches!(
                    before_content.content_state,
                    Some(EventContentState::Missing { .. })
                ));
                assert_eq!(before_content.content_rc, Some(1));
                assert_eq!(before_content.queue, vec![(Timestamp::ZERO, event_id)]);
                assert!(before_content.posts_by_time.is_empty());
                db.process_event_content(&content).await;
            }
            _ => unreachable!(),
        }

        let first = snapshot(&db, author, event_id, content_hash).await?;
        db.process_event_content(&content).await;
        db.process_event(&content.event).await;
        db.process_event_with_content(&content).await;
        assert_eq!(
            snapshot(&db, author, event_id, content_hash).await?,
            first,
            "{scenario}: duplicate public calls changed lifecycle or projections"
        );

        drop(db);
        let reopened = Database::open(&path, author).await.boxed()?;
        assert_eq!(
            snapshot(&reopened, author, event_id, content_hash).await?,
            first,
            "{scenario}: reopen changed the terminal observation"
        );
        drop(reopened);

        force_total_replay(&path)?;
        let replayed = Database::open(&path, author).await.boxed()?;
        let after_replay = snapshot(&replayed, author, event_id, content_hash).await?;
        assert_eq!(after_replay, first, "{scenario}: total replay diverged");
        snapshots.push(after_replay);
    }

    assert!(snapshots.windows(2).all(|pair| pair[0] == pair[1]));
    let expected = &snapshots[0];
    assert!(expected.event_present);
    assert_eq!(
        expected.content.as_deref(),
        content.content.as_ref().map(AsRef::as_ref)
    );
    assert_eq!(expected.content_state, None);
    assert!(expected.queue.is_empty());
    assert_eq!(expected.content_rc, Some(1));
    assert_eq!(expected.event_receipts, vec![event_id]);
    assert_eq!(
        expected.posts_by_time,
        vec![(content.timestamp(), event_id)]
    );
    assert_eq!(expected.post_receipts, vec![event_id]);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn public_content_ingestion_preserves_terminal_states() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([92; 32]);
    let author = secret.id();

    let deleted = post(secret, "deleted before content");
    let deleted_id = deleted.event_id().to_short();
    let deleted_hash = deleted.content_hash();
    let delete = deletion(secret, deleted.event_id(), deleted.event_id());
    let (_deleted_dir, deleted_db) = crate::tests::temp_db(author).await?;
    deleted_db.process_event(&delete).await;
    deleted_db.process_event_content(&deleted).await;
    let deleted_before = snapshot(&deleted_db, author, deleted_id, deleted_hash).await?;
    deleted_db.process_event_content(&deleted).await;
    assert_eq!(
        snapshot(&deleted_db, author, deleted_id, deleted_hash).await?,
        deleted_before
    );
    assert!(matches!(
        deleted_before.content_state,
        Some(EventContentState::Deleted { .. })
    ));
    assert_eq!(deleted_before.content, None);
    assert!(deleted_before.queue.is_empty());
    assert!(deleted_before.posts_by_time.is_empty());
    assert!(deleted_before.post_receipts.is_empty());

    let pruned = verified_content(
        secret,
        EventKind::RAW,
        EventContentRaw::new(vec![0x5a; Database::MAX_CONTENT_LEN as usize + 1]),
        None,
    );
    let pruned_id = pruned.event_id().to_short();
    let pruned_hash = pruned.content_hash();
    let (_pruned_dir, pruned_db) = crate::tests::temp_db(author).await?;
    pruned_db.process_event_content(&pruned).await;
    let pruned_before = snapshot(&pruned_db, author, pruned_id, pruned_hash).await?;
    pruned_db.process_event_content(&pruned).await;
    assert_eq!(
        snapshot(&pruned_db, author, pruned_id, pruned_hash).await?,
        pruned_before
    );
    assert_eq!(pruned_before.content_state, Some(EventContentState::Pruned));
    assert_eq!(pruned_before.content, None);
    assert!(pruned_before.queue.is_empty());

    let invalid = verified_content(
        secret,
        EventKind::SOCIAL_POST,
        EventContentRaw::new(vec![0xff]),
        None,
    );
    let invalid_id = invalid.event_id().to_short();
    let invalid_hash = invalid.content_hash();
    let (_invalid_dir, invalid_db) = crate::tests::temp_db(author).await?;
    invalid_db.process_event_content(&invalid).await;
    let invalid_before = snapshot(&invalid_db, author, invalid_id, invalid_hash).await?;
    invalid_db.process_event_content(&invalid).await;
    assert_eq!(
        snapshot(&invalid_db, author, invalid_id, invalid_hash).await?,
        invalid_before
    );
    assert_eq!(
        invalid_before.content_state,
        Some(EventContentState::Invalid)
    );
    assert_eq!(invalid_before.content, None);
    assert!(invalid_before.queue.is_empty());
    assert!(invalid_before.posts_by_time.is_empty());
    assert!(invalid_before.post_receipts.is_empty());
    assert_eq!(
        invalid_db
            .read_with(|tx| Ok(tx.open_table(&content_store::TABLE)?.range(..)?.count()))
            .await?,
        0
    );

    Ok(())
}
