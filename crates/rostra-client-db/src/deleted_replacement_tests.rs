use std::borrow::Cow;

use itertools::Itertools as _;
use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{Event, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;

use crate::event::{ContentStoreRecord, EventContentState};
use crate::{
    Database, IdsDataUsageRecord, content_rc, content_store, db_version, events_content_missing,
    events_content_state, social_news_rank_by_post_id, social_posts, social_posts_by_received_at,
    social_posts_by_time, social_posts_replaced_by, social_posts_replaces,
    social_posts_self_mention,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Envelope(usize),
    Payload(usize),
}

#[derive(Clone)]
struct ReplacementChain {
    events: [VerifiedEventContent; 3],
    self_id: RostraId,
}

impl ReplacementChain {
    fn edit() -> Self {
        Self::new(format!(
            "edited; hello <rostra:{}>",
            RostraIdSecretKey::from_bytes([42; 32]).id()
        ))
    }

    fn blank_delete() -> Self {
        Self::new(String::new())
    }

    fn oversized_edit() -> Self {
        Self::new("x".repeat(Database::MAX_CONTENT_LEN as usize))
    }

    fn exact_limit_edit() -> Self {
        let target_len = Database::MAX_CONTENT_LEN as usize;
        let mut body_len = target_len;
        loop {
            let chain = Self::new("x".repeat(body_len));
            let content_len = chain.events[1].content_len() as usize;
            if content_len == target_len {
                return chain;
            }
            body_len -= content_len
                .checked_sub(target_len)
                .expect("serialized social post unexpectedly below target length");
        }
    }

    fn new(intermediate_body: String) -> Self {
        let author_secret = RostraIdSecretKey::from_bytes([41; 32]);
        let self_id = RostraIdSecretKey::from_bytes([42; 32]).id();
        let original = social_post(
            author_secret,
            Timestamp::from(1),
            None,
            None,
            content_kind::SocialPost::new_text("original".to_owned(), None, Default::default()),
        );
        let original_id = original.event_id();
        let intermediate = social_post(
            author_secret,
            Timestamp::from(2),
            Some(original_id),
            Some(original_id),
            content_kind::SocialPost::new_text(
                intermediate_body,
                Some(ExternalEventId::new(author_secret.id(), original_id)),
                Default::default(),
            )
            .with_news_fields(None, Some("intermediate news".to_owned())),
        );
        let intermediate_id = intermediate.event_id();
        let newest = social_post(
            author_secret,
            Timestamp::from(3),
            Some(intermediate_id),
            Some(intermediate_id),
            content_kind::SocialPost::new_text("newest".to_owned(), None, Default::default()),
        );

        Self {
            events: [original, intermediate, newest],
            self_id,
        }
    }

    fn ids(&self) -> [ShortEventId; 3] {
        self.events
            .each_ref()
            .map(|event| event.event_id().to_short())
    }
}

fn social_post(
    secret: RostraIdSecretKey,
    timestamp: Timestamp,
    parent_prev: Option<rostra_core::EventId>,
    replaced: Option<rostra_core::EventId>,
    content: content_kind::SocialPost,
) -> VerifiedEventContent {
    let content = content
        .serialize_cbor()
        .expect("social-post content must serialize");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(
            timestamp
                .to_offset_date_time()
                .expect("valid test timestamp"),
        )
        .content(&content)
        .maybe_parent_prev(parent_prev.map(Into::into))
        .maybe_delete(replaced.map(Into::into))
        .build();
    let signed = event.signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), signed).expect("event must verify");

    VerifiedEventContent::assume_verified(event, content)
}

fn valid_delivery_schedules() -> Vec<Vec<Delivery>> {
    let deliveries = [
        Delivery::Envelope(0),
        Delivery::Payload(0),
        Delivery::Envelope(1),
        Delivery::Payload(1),
        Delivery::Envelope(2),
        Delivery::Payload(2),
    ];

    deliveries
        .into_iter()
        .permutations(deliveries.len())
        .filter(|schedule| {
            (0..3).all(|index| {
                let envelope = schedule
                    .iter()
                    .position(|delivery| *delivery == Delivery::Envelope(index))
                    .expect("envelope delivery exists");
                let payload = schedule
                    .iter()
                    .position(|delivery| *delivery == Delivery::Payload(index))
                    .expect("payload delivery exists");
                envelope < payload
            })
        })
        .collect()
}

async fn deliver(db: &Database, chain: &ReplacementChain, schedule: &[Delivery]) {
    for delivery in schedule {
        match *delivery {
            Delivery::Envelope(index) => {
                db.process_event(&chain.events[index].event).await;
            }
            Delivery::Payload(index) => {
                db.process_event_content(&chain.events[index]).await;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    fn from(record: IdsDataUsageRecord) -> Self {
        let IdsDataUsageRecord {
            current_metadata_size,
            total_metadata_size,
            current_metadata_num,
            total_metadata_num,
            current_content_size,
            total_content_size,
            current_payload_num,
            total_payload_num,
            missing_payload_size,
            missing_payload_num,
            deleted_payload_size,
            deleted_payload_num,
            pruned_payload_size,
            pruned_payload_num,
            invalid_payload_size,
            invalid_payload_num,
        } = record;
        Self {
            current_metadata_size,
            total_metadata_size,
            current_metadata_num,
            total_metadata_num,
            current_content_size,
            total_content_size,
            current_payload_num,
            total_payload_num,
            missing_payload_size,
            missing_payload_num,
            deleted_payload_size,
            deleted_payload_num,
            pruned_payload_size,
            pruned_payload_num,
            invalid_payload_size,
            invalid_payload_num,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TableSnapshot {
    replaced_by: Vec<(ShortEventId, ShortEventId)>,
    replaces: Vec<(ShortEventId, ShortEventId)>,
    states: [Option<EventContentState>; 3],
    content_rcs: [u64; 3],
    missing_queue: Vec<(Timestamp, ShortEventId)>,
    time_index: Vec<ShortEventId>,
    original_reply_count: u64,
    intermediate_mention: bool,
    intermediate_news: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct SemanticSnapshot {
    tables: TableSnapshot,
    visible_posts: Vec<ShortEventId>,
    resolved_original: Option<ShortEventId>,
    usage: UsageSnapshot,
}

async fn semantic_snapshot(
    db: &Database,
    chain: &ReplacementChain,
) -> BoxedErrorResult<SemanticSnapshot> {
    let [original_id, intermediate_id, newest_id] = chain.ids();
    let author = chain.events[0].author();
    let tables = db
        .read_with(|tx| {
            let replaced_by = tx
                .open_table(&social_posts_replaced_by::TABLE)?
                .range(
                    &(author, ShortEventId::ZERO, ShortEventId::ZERO)
                        ..=&(author, ShortEventId::MAX, ShortEventId::MAX),
                )?
                .map(|entry| {
                    entry.map(|(key, _)| {
                        let (_, old_id, new_id) = key.value();
                        (old_id, new_id)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let replaces = tx
                .open_table(&social_posts_replaces::TABLE)?
                .range(
                    &(author, ShortEventId::ZERO, ShortEventId::ZERO)
                        ..=&(author, ShortEventId::MAX, ShortEventId::MAX),
                )?
                .map(|entry| {
                    entry.map(|(key, _)| {
                        let (_, new_id, old_id) = key.value();
                        (new_id, old_id)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let state_table = tx.open_table(&events_content_state::TABLE)?;
            let states = [original_id, intermediate_id, newest_id].map(|event_id| {
                state_table
                    .get(&event_id)
                    .expect("state read must succeed")
                    .map(|state| state.value())
            });
            let rc_table = tx.open_table(&content_rc::TABLE)?;
            let content_rcs = chain.events.each_ref().map(|event| {
                Database::get_content_rc_tx(event.content_hash(), &rc_table)
                    .expect("RC read must succeed")
            });
            let missing_queue = tx
                .open_table(&events_content_missing::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value()))
                .collect::<Result<Vec<_>, _>>()?;
            let time_index = tx
                .open_table(&social_posts_by_time::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value().1))
                .collect::<Result<Vec<_>, _>>()?;
            let original_record = tx
                .open_table(&social_posts::TABLE)?
                .get(&original_id)?
                .map(|record| record.value())
                .unwrap_or_default();
            let intermediate_mention = tx
                .open_table(&social_posts_self_mention::TABLE)?
                .get(&intermediate_id)?
                .is_some();
            let intermediate_news = tx
                .open_table(&social_news_rank_by_post_id::TABLE)?
                .get(&ExternalEventId::new(author, intermediate_id))?
                .is_some();

            Ok(TableSnapshot {
                replaced_by,
                replaces,
                states,
                content_rcs,
                missing_queue,
                time_index,
                original_reply_count: original_record.reply_count,
                intermediate_mention,
                intermediate_news,
            })
        })
        .await?;
    let resolved_original = db
        .get_social_post(original_id)
        .await
        .map(|post| post.event_id);
    let (visible_posts, _) = db.paginate_social_posts_rev(None, 10, |_| true).await;
    let usage = db.get_data_usage(author).await.into();

    Ok(SemanticSnapshot {
        tables,
        visible_posts: visible_posts
            .into_iter()
            .map(|post| post.event_id)
            .collect(),
        resolved_original,
        usage,
    })
}

fn assert_expected_bookkeeping(snapshot: &SemanticSnapshot, chain: &ReplacementChain) {
    let [original, intermediate, newest] = chain.events.each_ref();
    assert_eq!(snapshot.tables.content_rcs, [0, 0, 1]);
    assert!(snapshot.tables.missing_queue.is_empty());
    assert_eq!(snapshot.usage.current_metadata_num, 3);
    assert_eq!(snapshot.usage.total_metadata_num, 3);
    assert_eq!(
        snapshot.usage.current_metadata_size,
        snapshot.usage.total_metadata_size
    );
    assert_eq!(snapshot.usage.current_payload_num, 1);
    assert_eq!(
        snapshot.usage.current_content_size,
        u64::from(newest.content_len())
    );
    assert_eq!(snapshot.usage.deleted_payload_num, 2);
    assert_eq!(
        snapshot.usage.deleted_payload_size,
        u64::from(original.content_len()) + u64::from(intermediate.content_len())
    );
    assert_eq!(snapshot.usage.total_payload_num, 3);
    assert_eq!(
        snapshot.usage.total_content_size,
        snapshot.usage.current_content_size + snapshot.usage.deleted_payload_size
    );
    assert_eq!(snapshot.usage.missing_payload_size, 0);
    assert_eq!(snapshot.usage.missing_payload_num, 0);
    assert_eq!(snapshot.usage.pruned_payload_size, 0);
    assert_eq!(snapshot.usage.pruned_payload_num, 0);
    assert_eq!(snapshot.usage.invalid_payload_size, 0);
    assert_eq!(snapshot.usage.invalid_payload_num, 0);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn deleted_intermediate_lineage_converges_for_all_valid_deliveries() -> BoxedErrorResult<()> {
    let schedules = valid_delivery_schedules();
    assert_eq!(schedules.len(), 90);
    let chain = ReplacementChain::edit();
    let [original_id, intermediate_id, newest_id] = chain.ids();
    let mut expected_edges = vec![(original_id, intermediate_id), (intermediate_id, newest_id)];
    expected_edges.sort_unstable();
    let mut expected_reverse = expected_edges
        .iter()
        .map(|(old_id, new_id)| (*new_id, *old_id))
        .collect::<Vec<_>>();
    expected_reverse.sort_unstable();
    let mut baseline = None;

    for schedule in &schedules {
        let db = Database::new_in_memory(chain.self_id).await.boxed()?;
        deliver(&db, &chain, schedule).await;
        let snapshot = semantic_snapshot(&db, &chain).await?;

        assert_eq!(snapshot.tables.replaced_by, expected_edges, "{schedule:?}");
        assert_eq!(snapshot.tables.replaces, expected_reverse, "{schedule:?}");
        assert_eq!(snapshot.resolved_original, Some(newest_id), "{schedule:?}");
        assert!(matches!(
            snapshot.tables.states[0],
            Some(EventContentState::Deleted { .. })
        ));
        assert!(matches!(
            snapshot.tables.states[1],
            Some(EventContentState::Deleted { .. })
        ));
        assert_eq!(snapshot.tables.states[2], None);
        assert_eq!(snapshot.tables.time_index, vec![newest_id]);
        assert_eq!(snapshot.visible_posts, vec![newest_id]);
        assert_eq!(snapshot.tables.original_reply_count, 0);
        assert!(!snapshot.tables.intermediate_mention);
        assert!(!snapshot.tables.intermediate_news);
        assert_expected_bookkeeping(&snapshot, &chain);

        if let Some(baseline) = &baseline {
            assert_eq!(&snapshot, baseline, "{schedule:?}");
        } else {
            baseline = Some(snapshot);
        }
    }

    Ok(())
}

async fn assert_predeleted_payload_is_lineage_only(
    chain: ReplacementChain,
    expect_new_edge: bool,
) -> BoxedErrorResult<()> {
    let [original_id, intermediate_id, newest_id] = chain.ids();
    let db = Database::new_in_memory(chain.self_id).await.boxed()?;
    let mut new_posts = db.new_posts_subscribe();
    let mut new_content = db.new_content_subscribe();
    deliver(&db, &chain, &[Delivery::Envelope(2), Delivery::Payload(2)]).await;
    let (_, intermediate_state) = db.process_event(&chain.events[1].event).await;
    assert_eq!(intermediate_state, crate::ProcessEventState::Deleted);
    db.process_event(&chain.events[0].event).await;
    let before = semantic_snapshot(&db, &chain).await?;
    assert!(before.tables.missing_queue.is_empty());
    assert!(
        !db.read_with(|tx| {
            Ok(tx
                .open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .any(|entry| {
                    entry
                        .map(|(_, event_id)| event_id.value() == intermediate_id)
                        .unwrap_or(false)
                }))
        })
        .await?,
        "predeleted envelope must not enter the reception index"
    );
    assert_eq!(
        new_content
            .try_recv()
            .expect("newest content notification")
            .event_id()
            .to_short(),
        newest_id
    );

    db.process_event_content(&chain.events[1]).await;
    db.process_event_content(&chain.events[1]).await;
    let after = semantic_snapshot(&db, &chain).await?;

    assert_eq!(after.tables.states, before.tables.states);
    assert_eq!(after.tables.content_rcs, before.tables.content_rcs);
    assert_eq!(after.tables.missing_queue, before.tables.missing_queue);
    assert_eq!(after.tables.time_index, before.tables.time_index);
    assert_eq!(after.tables.original_reply_count, 0);
    assert_eq!(
        after.tables.intermediate_mention,
        before.tables.intermediate_mention
    );
    assert_eq!(
        after.tables.intermediate_news,
        before.tables.intermediate_news
    );
    assert_eq!(after.visible_posts, before.visible_posts);
    assert_eq!(after.usage, before.usage);
    let mut expected_replaced_by = vec![(intermediate_id, newest_id)];
    let mut expected_replaces = vec![(newest_id, intermediate_id)];
    if expect_new_edge {
        expected_replaced_by.push((original_id, intermediate_id));
        expected_replaces.push((intermediate_id, original_id));
    }
    expected_replaced_by.sort_unstable();
    expected_replaces.sort_unstable();
    assert_eq!(
        before.tables.replaced_by,
        vec![(intermediate_id, newest_id)]
    );
    assert_eq!(before.tables.replaces, vec![(newest_id, intermediate_id)]);
    assert_eq!(after.tables.replaced_by, expected_replaced_by);
    assert_eq!(after.tables.replaces, expected_replaces);
    assert_eq!(
        after.resolved_original,
        expect_new_edge.then_some(newest_id)
    );
    assert_eq!(
        new_posts
            .try_recv()
            .expect("newest post notification")
            .0
            .event_id()
            .to_short(),
        newest_id
    );
    assert!(matches!(
        new_posts.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        new_content.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert!(db.get_event_content(intermediate_id).await.is_none());
    assert!(
        db.read_with(|tx| {
            Ok(tx
                .open_table(&content_store::TABLE)?
                .get(&chain.events[1].content_hash())?
                .is_none())
        })
        .await?,
        "supplying Deleted content must not retain its bytes"
    );
    assert!(
        !db.read_with(|tx| {
            Ok(tx
                .open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .any(|entry| {
                    entry
                        .map(|(_, event_id)| event_id.value() == intermediate_id)
                        .unwrap_or(false)
                }))
        })
        .await?,
        "supplying Deleted content must not add a reception row"
    );

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn supplied_predeleted_edit_changes_only_lineage() -> BoxedErrorResult<()> {
    assert_predeleted_payload_is_lineage_only(ReplacementChain::edit(), true).await
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn supplied_predeleted_blank_delete_changes_nothing() -> BoxedErrorResult<()> {
    assert_predeleted_payload_is_lineage_only(ReplacementChain::blank_delete(), false).await
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn over_limit_deleted_edit_derives_no_lineage_from_retained_or_supplied_bytes()
-> BoxedErrorResult<()> {
    let chain = ReplacementChain::oversized_edit();
    let [original_id, intermediate_id, _] = chain.ids();
    let db = Database::new_in_memory(chain.self_id).await.boxed()?;
    db.write_with(|tx| {
        let content = chain.events[1]
            .content
            .as_ref()
            .expect("intermediate content exists")
            .clone();
        tx.open_table(&content_store::TABLE)?.insert(
            &chain.events[1].content_hash(),
            &ContentStoreRecord(Cow::Owned(content)),
        )?;
        Ok(())
    })
    .await?;
    deliver(&db, &chain, &[Delivery::Envelope(2), Delivery::Payload(2)]).await;
    let (_, state) = db.process_event(&chain.events[1].event).await;
    assert_eq!(state, crate::ProcessEventState::Deleted);
    db.process_event_content(&chain.events[1]).await;

    let snapshot = semantic_snapshot(&db, &chain).await?;
    assert!(
        !snapshot
            .tables
            .replaced_by
            .contains(&(original_id, intermediate_id))
    );
    assert!(
        !snapshot
            .tables
            .replaces
            .contains(&(intermediate_id, original_id))
    );
    assert!(matches!(
        snapshot.tables.states[1],
        Some(EventContentState::Deleted { .. })
    ));

    Ok(())
}

fn force_total_replay(db_path: &std::path::Path) -> BoxedErrorResult<()> {
    let raw_db = redb_bincode::Database::from(redb::Database::open(db_path).boxed()?);
    let write_txn = raw_db.begin_write().boxed()?;
    write_txn
        .open_table(&db_version::TABLE)
        .boxed()?
        .insert(&(), &24)
        .boxed()?;
    write_txn.commit().boxed()?;
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn retained_deleted_edit_lineage_survives_reopen_gc_and_total_replay() -> BoxedErrorResult<()>
{
    let chain = ReplacementChain::edit();
    let db_dir = tempfile::tempdir()?;
    let db_path = db_dir.path().join("db.redb");

    let expected = {
        let db = Database::open(&db_path, chain.self_id).await.boxed()?;
        db.write_with(|tx| {
            let content = chain.events[1]
                .content
                .as_ref()
                .expect("intermediate content exists")
                .clone();
            tx.open_table(&content_store::TABLE)?.insert(
                &chain.events[1].content_hash(),
                &ContentStoreRecord(Cow::Owned(content)),
            )?;
            Ok(())
        })
        .await?;
        deliver(
            &db,
            &chain,
            &[
                Delivery::Envelope(2),
                Delivery::Payload(2),
                Delivery::Envelope(1),
                Delivery::Envelope(0),
            ],
        )
        .await;
        let snapshot = semantic_snapshot(&db, &chain).await?;
        assert_eq!(
            snapshot.resolved_original,
            Some(chain.events[2].event_id().to_short())
        );
        assert_expected_bookkeeping(&snapshot, &chain);

        db.write_with(|tx| {
            tx.open_table(&content_store::TABLE)?
                .remove(&chain.events[1].content_hash())?;
            Ok(())
        })
        .await?;
        snapshot
    };
    let reopened = {
        let db = Database::open(&db_path, chain.self_id).await.boxed()?;
        semantic_snapshot(&db, &chain).await?
    };
    assert_eq!(reopened, expected);

    force_total_replay(&db_path)?;
    let replayed = {
        let db = Database::open(&db_path, chain.self_id).await.boxed()?;
        semantic_snapshot(&db, &chain).await?
    };
    assert_eq!(replayed, expected);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn total_replay_removes_legacy_exact_limit_deleted_edit_lineage() -> BoxedErrorResult<()> {
    let chain = ReplacementChain::exact_limit_edit();
    let [original_id, intermediate_id, _] = chain.ids();
    let db_dir = tempfile::tempdir()?;
    let db_path = db_dir.path().join("db.redb");

    let expected = {
        let db = Database::open(&db_path, chain.self_id).await.boxed()?;
        deliver(
            &db,
            &chain,
            &[
                Delivery::Envelope(2),
                Delivery::Payload(2),
                Delivery::Envelope(1),
                Delivery::Payload(1),
                Delivery::Envelope(0),
            ],
        )
        .await;
        let expected = semantic_snapshot(&db, &chain).await?;
        assert_expected_bookkeeping(&expected, &chain);
        assert!(
            !expected
                .tables
                .replaced_by
                .contains(&(original_id, intermediate_id))
        );

        db.write_with(|tx| {
            tx.open_table(&social_posts_replaced_by::TABLE)?
                .insert(&(chain.self_id, original_id, intermediate_id), &())?;
            tx.open_table(&social_posts_replaces::TABLE)?
                .insert(&(chain.self_id, intermediate_id, original_id), &())?;
            Ok(())
        })
        .await?;
        expected
    };

    force_total_replay(&db_path)?;
    let replayed = {
        let db = Database::open(&db_path, chain.self_id).await.boxed()?;
        semantic_snapshot(&db, &chain).await?
    };
    assert_eq!(replayed, expected);

    Ok(())
}
