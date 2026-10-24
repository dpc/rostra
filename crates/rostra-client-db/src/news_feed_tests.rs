use std::collections::BTreeSet;

use rostra_core::event::SocialPost;
use rostra_core::id::RostraIdSecretKey;
use rostra_core::{ExternalEventId, Timestamp};

use crate::news_request_tests::{ingest, input, news, signed};
use crate::{Database, DbError};

#[tokio::test(flavor = "multi_thread")]
async fn deeper_than_200_exact_static_traversal_metadata_and_unrelated_writes() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let mut expected = BTreeSet::new();
    for n in 0..237 {
        let post = news(secret, 1, &input(n.to_string()), n + 1);
        expected.insert(post.event_id());
        ingest(&db, &post).await?;
    }
    let first = db.paginate_news_ranked(None, 17).await?;
    let cursor = first.next_cursor.clone().unwrap();
    let json = serde_json::to_string(&cursor)?;
    let cursor = serde_json::from_str(&json)?;
    let unrelated = signed(
        secret,
        &SocialPost::new_text("ordinary".into(), None, Default::default()),
        300,
        None,
    );
    ingest(&db, &unrelated).await?;
    let mut got: BTreeSet<_> = first.rows.iter().map(|row| row.target.1).collect();
    let mut cursor = Some(cursor);
    while cursor.is_some() {
        let page = db.paginate_news_ranked(cursor, 17).await?;
        for row in page.rows {
            assert!(got.insert(row.target.1));
        }
        cursor = page.next_cursor;
    }
    assert_eq!(got, expected);
    assert!(matches!(
        db.paginate_news_ranked(None, 101).await,
        Err(DbError::NewsPageLimit)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_materialization_rank_change_delete_replace_rebuild_reopen() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("feed.redb");
    let db = Database::open(&path, secret.id()).await?;
    let a = news(secret, 1, &input("a"), 1);
    let b = news(secret, 2, &input("b"), 2);
    ingest(&db, &a).await?;
    ingest(&db, &b).await?;
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    let c = news(secret, 3, &input("c"), 3);
    // Envelope-only ingestion has no membership effect.
    db.try_process_event(&c.event).await?;
    assert!(
        db.paginate_news_ranked(Some(cursor.clone()), 1)
            .await
            .is_ok()
    );
    ingest(&db, &c).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    let id = ExternalEventId::new(secret.id(), a.event_id());
    db.write_with(|tx| Database::recalculate_news_post_score_tx(id, Timestamp::from(100), tx))
        .await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    // An unchanged recalculation does not expire cursors.
    db.write_with(|tx| Database::recalculate_news_post_score_tx(id, Timestamp::from(100), tx))
        .await?;
    db.paginate_news_ranked(Some(cursor.clone()), 1).await?;
    let edit = signed(
        secret,
        &input("edited").to_social_post()?,
        4,
        Some(a.event_id()),
    );
    ingest(&db, &edit).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    assert!(
        db.paginate_news_ranked(None, 100)
            .await?
            .rows
            .iter()
            .any(|row| row.target.1 == edit.event_id())
    );
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    let deletion = signed(
        secret,
        &SocialPost::new_text("".into(), None, Default::default()),
        5,
        Some(b.event_id()),
    );
    ingest(&db, &deletion).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    db.write_with(|tx| Database::prepare_total_migration(tx, 37))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    // Aging-out is another membership mutation.
    db.write_with(|tx| {
        Database::recalculate_news_post_score_tx(
            ExternalEventId::new(secret.id(), c.event_id()),
            Timestamp::from(super::news::NEWS_MAX_AGE_SECS + 1000),
            tx,
        )
    })
    .await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_overflow_rolls_back_and_budgets_never_mean_exhaustion() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let post = news(secret, 1, &input("a"), 1);
    db.write_with(|tx| {
        tx.open_table(&crate::social_news_generation::TABLE)?
            .insert(&(), &([1; 16], u64::MAX))?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        ingest(&db, &post).await,
        Err(DbError::NewsGenerationExhausted)
    ));
    assert!(db.get_event(post.event_id()).await.is_none());
    db.write_with(|tx| {
        tx.open_table(&crate::social_news_generation::TABLE)?
            .remove(&())?;
        let mut ranks = tx.open_table(&crate::social_news_rank_by_score::TABLE)?;
        for i in 0..20001 {
            ranks.insert(
                &(
                    Database::news_score_key(i),
                    ExternalEventId::new(secret.id(), post.event_id()),
                ),
                &(),
            )?;
        }
        Ok(())
    })
    .await?;
    let eligible = news(secret, 2, &input("eligible beyond sparse spans"), 2);
    ingest(&db, &eligible).await?;
    db.write_with(|tx| {
        db.upsert_social_news_rank_tx(
            ExternalEventId::new(secret.id(), eligible.event_id()),
            Timestamp::from(2),
            -1,
            tx,
        )
    })
    .await?;
    let first = db.paginate_news_ranked(None, 1).await?;
    assert_eq!(first.stop, crate::news::NewsFeedStop::ScanBudget);
    assert_eq!(
        (
            first.rows.len(),
            first.raw_rows_scanned,
            first.unsupported_rows
        ),
        (0, 10000, 10000)
    );
    let first_cursor = serde_json::to_string(&first.next_cursor)?;
    let second = db.paginate_news_ranked(first.next_cursor, 1).await?;
    assert_eq!(second.stop, crate::news::NewsFeedStop::ScanBudget);
    assert_ne!(first_cursor, serde_json::to_string(&second.next_cursor)?);
    let third = db.paginate_news_ranked(second.next_cursor, 1).await?;
    assert_eq!(third.stop, crate::news::NewsFeedStop::End);
    assert_eq!(third.rows[0].target.1, eligible.event_id());
    assert_eq!(third.unsupported_rows, 1);
    let db = Database::new_in_memory(secret.id()).await?;
    // Oversized wire-valid rows are explicitly outside supported policy and
    // must not stall the next supported row, even at the start of a scan.
    for n in 1..=3 {
        let huge = news(secret, n, &input("x".repeat(1_000_001)), 1);
        ingest(&db, &huge).await?;
    }
    ingest(&db, &eligible).await?;
    db.write_with(|tx| {
        db.upsert_social_news_rank_tx(
            ExternalEventId::new(secret.id(), eligible.event_id()),
            Timestamp::from(2),
            -1,
            tx,
        )
    })
    .await?;
    let page = db.paginate_news_ranked(None, 1).await?;
    assert_eq!(page.stop, crate::news::NewsFeedStop::End);
    assert_eq!(
        (
            page.oversized_rows,
            page.unsupported_rows,
            page.raw_rows_scanned
        ),
        (3, 3, 4)
    );
    assert_eq!(page.rows[0].target.1, eligible.event_id());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn byte_budget_continuation_does_not_consume_the_unexamined_row() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let request = input("x".repeat(950_000));
    let mut expected = BTreeSet::new();
    for n in 1..=18 {
        let row = news(secret, n, &request, 1);
        expected.insert(row.event_id());
        ingest(&db, &row).await?;
    }
    let first = db.paginate_news_ranked(None, 100).await?;
    assert_eq!(first.stop, crate::news::NewsFeedStop::PayloadBudget);
    assert_eq!(first.rows.len(), 17);
    let second = db.paginate_news_ranked(first.next_cursor, 100).await?;
    assert_eq!(second.stop, crate::news::NewsFeedStop::End);
    assert_eq!(second.rows.len(), 1);
    let got: BTreeSet<_> = first
        .rows
        .iter()
        .chain(&second.rows)
        .map(|row| row.target.1)
        .collect();
    assert_eq!(got, expected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_vote_projection_fails_feed_instead_of_displaying_clear() -> anyhow::Result<()> {
    use rostra_core::event::EventKind;

    use crate::vote_request::{VoteRequest, VoteRequestId};
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = news(secret, 1, &input("target"), 1);
    ingest(&db, &target).await?;
    let request = VoteRequest {
        target: (secret.id(), target.event_id()),
        value: Some(true),
    };
    let vote = signed(secret, &request.to_social_vote(), 2, None);
    db.commit_vote(
        VoteRequestId([1; 16]),
        &request,
        &vote,
        &mut None,
        &mut None,
    )
    .await?;
    let target_id = ExternalEventId::new(secret.id(), target.event_id());
    let key = (
        secret.id(),
        EventKind::SOCIAL_VOTE,
        Database::social_vote_aux_key(target_id),
    );
    let original = db
        .read_with(|tx| {
            Ok(tx
                .open_table(&crate::events_singletons_new::TABLE)?
                .get(&key)?
                .unwrap()
                .value())
        })
        .await?;
    db.write_with(|tx| {
        let mut row = original.clone();
        row.inner.social_vote = None;
        tx.open_table(&crate::events_singletons_new::TABLE)?
            .insert(&key, &row)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.paginate_news_ranked(None, 1).await,
        Err(DbError::MissingVoteSingletonProjection { .. })
    ));
    db.write_with(|tx| {
        let mut row = original.clone();
        row.inner.social_vote.as_mut().unwrap().target =
            ExternalEventId::new(secret.id(), rostra_core::EventId::from_bytes([5; 32]));
        tx.open_table(&crate::events_singletons_new::TABLE)?
            .insert(&key, &row)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.paginate_news_ranked(None, 1).await,
        Err(DbError::InvalidVoteSingletonProjection { .. })
    ));
    db.write_with(|tx| {
        let mut row = original.clone();
        row.inner.social_vote.as_mut().unwrap().target =
            ExternalEventId::new(RostraIdSecretKey::generate().id(), target.event_id());
        tx.open_table(&crate::events_singletons_new::TABLE)?
            .insert(&key, &row)?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.paginate_news_ranked(None, 1).await?.rows[0].self_vote,
        None
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_rank_order_boundaries_ties_and_legacy_index_upgrade() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("rank-order.redb");
    let db = Database::open(&path, secret.id()).await?;
    let scores = [
        i64::MIN,
        -65536,
        -257,
        -256,
        -128,
        -127,
        -1,
        0,
        0,
        1,
        127,
        128,
        255,
        256,
        65535,
        65536,
        i64::MAX,
    ];
    let mut expected = Vec::new();
    for (n, score) in scores.into_iter().enumerate() {
        let post = news(secret, n as u8, &input(n.to_string()), 1);
        ingest(&db, &post).await?;
        let id = ExternalEventId::new(secret.id(), post.event_id());
        db.write_with(|tx| db.upsert_social_news_rank_tx(id, Timestamp::from(1), score, tx))
            .await?;
        expected.push((score, id));
    }
    expected.sort();
    expected.reverse();
    check_rank_order(&db, &expected).await?;
    db.read_with(|tx| {
        // The actual stored key starts with exactly eight ordered score bytes;
        // no integer varint/zigzag or host endianness participates.
        let raw = tx
            .as_raw()
            .open_table(crate::social_news_rank_by_score::TABLE.as_raw())?;
        let typed = tx.open_table(&crate::social_news_rank_by_score::TABLE)?;
        for (raw, typed) in raw.range::<&[u8]>(..)?.zip(typed.range(..)?) {
            let raw = raw?.0;
            let typed = typed?.0.value();
            assert_eq!(&raw.value()[..8], &typed.0);
        }
        Ok(())
    })
    .await?;
    let legacy = redb_bincode::TableDefinition::<(i64, ExternalEventId), ()>::new(
        "social_news_rank_by_score",
    );
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                "social_news_rank_by_score",
            ))?;
        let mut index = tx.open_table(&legacy)?;
        for row in &expected {
            index.insert(row, &())?;
        }
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &36)?;
        Ok(())
    })
    .await?;
    let aborted = db
        .write_with(|tx| {
            Database::handle_db_ver_migrations(tx)?;
            Err::<(), _>(DbError::NewsCandidate)
        })
        .await;
    assert!(aborted.is_err());
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&crate::db_version::TABLE)?
                .get(&())?
                .unwrap()
                .value(),
            36
        );
        let index = tx.open_table(&legacy)?;
        for row in &expected {
            assert!(index.get(row)?.is_some());
        }
        Ok(())
    })
    .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    check_rank_order(&db, &expected).await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    check_rank_order(&db, &expected).await?;
    Ok(())
}

async fn check_rank_order(
    db: &Database,
    expected: &[(i64, ExternalEventId)],
) -> anyhow::Result<()> {
    let mut actual = Vec::new();
    let mut cursor = None;
    loop {
        let page = db.paginate_news_ranked(cursor, 3).await?;
        actual.extend(
            page.rows
                .into_iter()
                .map(|row| (row.post.score, row.post.post_id)),
        );
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let mut actual = Vec::new();
    let mut cursor = None;
    loop {
        let (rows, next) = db.paginate_news_posts_by_rank_rev(cursor, 3).await;
        actual.extend(rows.into_iter().map(|row| (row.score, row.post_id)));
        cursor = next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn quota_pruning_invalidates_and_receipts_do_not_restore_payload() -> anyhow::Result<()> {
    use std::num::NonZeroUsize;

    use rostra_core::id::ToShort as _;
    use rostra_core::retention::RetentionPolicy;

    use crate::vote_request::{VoteRequest, VoteRequestId};

    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let a = news(remote, 1, &input("remote a"), 1);
    let b = news(remote, 2, &input("remote b"), 2);
    ingest(&db, &a).await?;
    ingest(&db, &b).await?;
    let request = VoteRequest {
        target: (remote.id(), a.event_id()),
        value: Some(true),
    };
    let vote = signed(local, &request.to_social_vote(), 10, None);
    db.commit_vote(
        VoteRequestId([1; 16]),
        &request,
        &vote,
        &mut None,
        &mut None,
    )
    .await?;
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    db.prune_quota_payload(crate::QuotaPruneRequest {
        id: a.event_id().to_short(),
        target: crate::QuotaPruneTarget::Processed,
        reason: crate::QuotaPruneReason::AuthorQuota,
        policy: RetentionPolicy::new(1, 1, 0, 0, 1, 10).unwrap(),
        clock: crate::RetentionClock::Trusted(Timestamp::from(200)),
    })
    .await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    assert_eq!(db.paginate_news_ranked(None, 100).await?.rows.len(), 1);
    assert_eq!(
        db.commit_vote(
            VoteRequestId([1; 16]),
            &request,
            &vote,
            &mut None,
            &mut None
        )
        .await?
        .event_id,
        vote.event_id()
    );
    assert!(db.get_event_content(a.event_id()).await.is_none());
    assert!(matches!(
        db.commit_vote(
            VoteRequestId([2; 16]),
            &request,
            &vote,
            &mut None,
            &mut None
        )
        .await,
        Err(DbError::NewsUnavailable)
    ));
    Ok(())
}
