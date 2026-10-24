use std::collections::BTreeSet;

use rostra_core::event::{EventExt as _, SocialPost, VerifiedEventContent};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, ExternalEventId, Timestamp};

use crate::news_request_tests::{ingest, signed};
use crate::notification::{
    NOTIFICATION_PAYLOAD_MAX, NotificationBounds, NotificationStop, NotificationUnread,
};
use crate::{Database, DbError};

fn bounds() -> NotificationBounds {
    NotificationBounds {
        rows: 20,
        raw_rows: 256,
        payload_bytes: 1024 * 1024,
    }
}

fn post(
    author: RostraIdSecretKey,
    time: u64,
    body: impl Into<String>,
    reply: Option<RostraId>,
) -> VerifiedEventContent {
    signed(
        author,
        &SocialPost::new_text(
            body.into(),
            reply.map(|id| ExternalEventId::new(id, EventId::from_bytes([1; 32]))),
            Default::default(),
        ),
        time,
        None,
    )
}

async fn ingest_at(db: &Database, post: &VerifiedEventContent, time: u64) -> crate::DbResult<()> {
    db.write_with(|tx| {
        db.process_event_tx(&post.event, Timestamp::from(time), tx)?;
        db.process_event_content_tx(post, Timestamp::from(time), tx)
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn web_membership_reasons_global_highwater_and_no_auto_ack() -> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let empty = db.paginate_notifications(None, bounds()).await?;
    let before = empty.highwater.unwrap();
    assert!(before.received_highwater().is_none());
    assert!(empty.rows.is_empty());
    assert_eq!(empty.stop, NotificationStop::End);
    let mention = format!("Hello <rostra:{}>!", local.id().to_short());
    let cases = [
        post(remote, 1, "reply", Some(local.id())),
        post(remote, 2, &mention, None),
        post(remote, 3, &mention, Some(local.id())),
        post(remote, 4, "other", Some(remote.id())),
        post(local, 5, &mention, Some(local.id())),
    ];
    for p in &cases {
        ingest(&db, p).await?;
    }
    let page = db.paginate_notifications(None, bounds()).await?;
    assert_eq!(page.rows.len(), 3);
    assert_eq!(page.rows[0].target, (remote.id(), cases[2].event_id()));
    assert_eq!(page.rows[0].reason.reply, Some(true));
    assert!(page.rows[0].reason.mention);
    assert_eq!(page.rows[1].reason.reply, Some(false));
    assert!(page.rows[1].reason.mention);
    assert!(!page.rows[2].reason.mention);
    assert_eq!(page.rows[2].reason.reply, Some(true));
    assert!(
        page.rows
            .windows(2)
            .all(|pair| pair[0].received > pair[1].received)
    );
    let latest = db.get_latest_social_post_received_at_cursor().await;
    let seen = page.highwater.unwrap();
    assert_eq!(seen.received_highwater(), latest);
    assert!(latest.unwrap() > page.rows[0].received);
    // Reading a page never changes host-owned seen state.
    for _ in 0..2 {
        assert_eq!(
            db.scan_notifications_unread(Some(before.clone()), None, bounds())
                .await?
                .state,
            NotificationUnread::Yes
        );
    }
    assert_eq!(
        db.scan_notifications_unread(Some(seen.clone()), None, bounds())
            .await?
            .state,
        NotificationUnread::No
    );
    let later = post(remote, 6, "later", Some(local.id()));
    ingest(&db, &later).await?;
    assert_eq!(
        db.scan_notifications_unread(Some(seen), None, bounds())
            .await?
            .state,
        NotificationUnread::Yes
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_sparse_ties_exclusive_progress_and_backdated_concurrent_arrivals()
-> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let mut expected = BTreeSet::new();
    for n in 0..237 {
        let p = post(
            remote,
            n + 1,
            n.to_string(),
            (n % 11 == 0).then_some(local.id()),
        );
        ingest(&db, &p).await?;
        if n % 11 == 0 {
            expected.insert(p.event_id());
        }
    }
    let small = NotificationBounds {
        raw_rows: 3,
        rows: 2,
        ..bounds()
    };
    let mut page = db.paginate_notifications(None, small).await?;
    assert_eq!(page.stop, NotificationStop::ScanBudget);
    assert!(page.rows.is_empty());
    let seen = page.highwater.clone().unwrap();
    let late = post(remote, 300, "backdated arrival", Some(local.id()));
    ingest_at(&db, &late, 1).await?;
    let mut got = BTreeSet::new();
    loop {
        assert!(page.raw_rows_scanned <= 3);
        for row in page.rows {
            assert!(got.insert(row.target.1));
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        // Transport roundtrip must retain the exclusive raw-row position.
        let cursor = serde_json::from_str(&serde_json::to_string(&cursor)?)?;
        page = db.paginate_notifications(Some(cursor), small).await?;
        assert!(page.highwater.is_none());
    }
    assert_eq!(got, expected);
    let mut cursor = None;
    loop {
        let unread = db
            .scan_notifications_unread(Some(seen.clone()), cursor, small)
            .await?;
        assert_ne!(unread.state, NotificationUnread::No);
        if unread.state == NotificationUnread::Yes {
            break;
        }
        cursor = Some(unread.next_cursor.expect("bounded unknown must progress"));
    }
    assert!(
        db.paginate_notifications(None, bounds())
            .await?
            .highwater
            .unwrap()
            .received_highwater()
            >= seen.received_highwater()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn delayed_payload_rollback_and_empty_refresh_keep_later_arrival_unread() -> anyhow::Result<()>
{
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let delayed = post(remote, 1, "late payload", Some(local.id()));
    db.try_process_event(&delayed.event).await?;
    ingest_at(&db, &post(local, 2, "own highwater", None), 500).await?;
    let first = db.paginate_notifications(None, bounds()).await?;
    assert!(first.rows.is_empty());
    let seen = first.highwater.unwrap();
    assert!(seen.received_highwater().is_some());
    // Failed materialization must not consume the durable sequence or expose a row.
    let failed = db
        .write_with(|tx| -> crate::DbResult<()> {
            db.process_event_content_tx(&delayed, Timestamp::from(3), tx)?;
            Err(DbError::NotificationInvariant)
        })
        .await;
    assert!(failed.is_err());
    let after = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    assert_eq!(after, seen);
    ingest_at(&db, &delayed, 3).await?;
    let page = db.paginate_notifications(None, bounds()).await?;
    assert!(page.rows[0].received < seen.received_highwater().unwrap());
    assert_eq!(
        db.scan_notifications_unread(Some(seen), None, bounds())
            .await?
            .state,
        NotificationUnread::Yes
    );
    // Replay of already-processed content never creates another arrival.
    let seen = page.highwater.unwrap();
    ingest_at(&db, &delayed, 900).await?;
    assert_eq!(
        db.scan_notifications_unread(Some(seen), None, bounds())
            .await?
            .state,
        NotificationUnread::No
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn payload_budget_progress_classification_and_terminal_unknown() -> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let unseen = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    let big = post(remote, 1, "x".repeat(1500), Some(local.id()));
    ingest(&db, &big).await?;
    let small = NotificationBounds {
        payload_bytes: 500,
        ..bounds()
    };
    let page = db.paginate_notifications(None, small).await?;
    assert!(page.rows.is_empty());
    assert_eq!(page.issues.len(), 1);
    assert_eq!(page.issues[0].target.1, big.event_id());
    assert_eq!(page.payload_bytes_read, 0);
    let unread = db
        .scan_notifications_unread(Some(unseen.clone()), None, small)
        .await?;
    assert_eq!(unread.state, NotificationUnread::Unknown);
    assert_eq!(unread.stop, NotificationStop::End);
    assert!(unread.next_cursor.is_none());
    assert_eq!(
        db.scan_notifications_unread(Some(unseen.clone()), None, bounds())
            .await?
            .state,
        NotificationUnread::Yes
    );
    let mention = post(
        remote,
        2,
        format!("<rostra:{}> {}", local.id().to_short(), "x".repeat(1500)),
        None,
    );
    ingest(&db, &mention).await?;
    let page = db.paginate_notifications(None, small).await?;
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].content.is_none());
    assert!(page.rows[0].reason.mention);
    assert_eq!(page.rows[0].reason.reply, None);
    assert_eq!(
        db.scan_notifications_unread(Some(unseen), None, small)
            .await?
            .state,
        NotificationUnread::Yes
    );
    // The same payload can fit alone but not together; leave the next row
    // unconsumed.
    let medium = NotificationBounds {
        payload_bytes: 2000,
        ..bounds()
    };
    let page = db.paginate_notifications(None, medium).await?;
    assert_eq!(page.stop, NotificationStop::PayloadBudget);
    assert_eq!(page.raw_rows_scanned, 1);
    let next = db.paginate_notifications(page.next_cursor, medium).await?;
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows[0].target.1, big.event_id());
    assert_eq!(next.stop, NotificationStop::End);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn unread_sparse_unknown_carries_uncertainty_and_binds_seen_and_frontier()
-> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let seen = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    for n in 0..3 {
        ingest(&db, &post(remote, n, "unrelated", None)).await?;
    }
    ingest(&db, &post(remote, 10, "x".repeat(500), None)).await?;
    let tiny = NotificationBounds {
        raw_rows: 1,
        payload_bytes: 100,
        ..bounds()
    };
    let page = db
        .scan_notifications_unread(Some(seen.clone()), None, tiny)
        .await?;
    assert_eq!(page.state, NotificationUnread::Unknown);
    assert_eq!(page.issues.len(), 1);
    let cursor = page.next_cursor.unwrap();
    assert!(matches!(
        db.scan_notifications_unread(None, Some(cursor.clone()), tiny)
            .await,
        Err(DbError::NotificationCursorMismatch)
    ));
    let mut next = Some(cursor.clone());
    loop {
        let page = db
            .scan_notifications_unread(Some(seen.clone()), next, tiny)
            .await?;
        assert_eq!(page.state, NotificationUnread::Unknown);
        if page.next_cursor.is_none() {
            assert_eq!(page.stop, NotificationStop::End);
            break;
        }
        next = page.next_cursor;
    }
    ingest(&db, &post(remote, 20, "new allocation", None)).await?;
    assert!(matches!(
        db.scan_notifications_unread(Some(seen), Some(cursor), tiny)
            .await,
        Err(DbError::NotificationCursorStale)
    ));
    // Sparse scans eventually prove No, but never before reaching End.
    let mut next = None;
    loop {
        let page = db
            .scan_notifications_unread(
                None,
                next,
                NotificationBounds {
                    raw_rows: 1,
                    ..bounds()
                },
            )
            .await?;
        if page.next_cursor.is_none() {
            assert_eq!(page.state, NotificationUnread::No);
            break;
        }
        assert_eq!(page.state, NotificationUnread::Unknown);
        next = page.next_cursor;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_scope_reopen_rebuild_foreign_and_older_backup() -> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("notifications.redb");
    let backup = dir.path().join("older.redb");
    let db = Database::open(&path, local.id()).await?;
    ingest(&db, &post(remote, 1, "one", Some(local.id()))).await?;
    ingest(&db, &post(remote, 2, "two", Some(local.id()))).await?;
    let first = db
        .paginate_notifications(
            None,
            NotificationBounds {
                rows: 1,
                ..bounds()
            },
        )
        .await?;
    let cursor = first.next_cursor.unwrap();
    let seen = first.highwater.unwrap();
    drop(db);
    std::fs::copy(&path, &backup)?;
    let db = Database::open(&path, local.id()).await?;
    let seen = serde_json::from_str(&serde_json::to_string(&seen)?)?;
    assert_eq!(
        db.paginate_notifications(Some(cursor.clone()), bounds())
            .await?
            .rows
            .len(),
        1
    );
    assert_eq!(
        db.scan_notifications_unread(Some(seen), None, bounds())
            .await?
            .state,
        NotificationUnread::No
    );
    ingest(&db, &post(remote, 3, "three", Some(local.id()))).await?;
    let newer = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    let old = Database::open(&backup, local.id()).await?;
    assert!(matches!(
        old.scan_notifications_unread(Some(newer.clone()), None, bounds())
            .await,
        Err(DbError::NotificationCursorStale)
    ));
    let same_account = Database::new_in_memory(local.id()).await?;
    assert!(matches!(
        same_account
            .paginate_notifications(Some(cursor.clone()), bounds())
            .await,
        Err(DbError::NotificationCursorStale)
    ));
    let other_account = Database::new_in_memory(remote.id()).await?;
    assert!(matches!(
        other_account
            .paginate_notifications(Some(cursor.clone()), bounds())
            .await,
        Err(DbError::NotificationCursorStale)
    ));
    db.write_with(|tx| Database::prepare_total_migration(tx, 38))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert!(matches!(
        db.paginate_notifications(Some(cursor), bounds()).await,
        Err(DbError::NotificationCursorStale)
    ));
    assert!(matches!(
        db.scan_notifications_unread(Some(newer), None, bounds())
            .await,
        Err(DbError::NotificationCursorStale)
    ));
    assert_eq!(
        db.paginate_notifications(None, bounds()).await?.rows.len(),
        3
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_validation() -> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    for invalid in [
        NotificationBounds {
            rows: 0,
            ..bounds()
        },
        NotificationBounds {
            rows: 101,
            ..bounds()
        },
        NotificationBounds {
            raw_rows: 0,
            ..bounds()
        },
        NotificationBounds {
            raw_rows: 4097,
            ..bounds()
        },
        NotificationBounds {
            payload_bytes: 0,
            ..bounds()
        },
        NotificationBounds {
            payload_bytes: NOTIFICATION_PAYLOAD_MAX + 1,
            ..bounds()
        },
    ] {
        assert!(matches!(
            db.paginate_notifications(None, invalid).await,
            Err(DbError::NotificationBounds)
        ));
        assert!(matches!(
            db.scan_notifications_unread(None, None, invalid).await,
            Err(DbError::NotificationBounds)
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_deep_lineage_large_mention_index_and_pruning() -> anyhow::Result<()> {
    use std::num::NonZeroUsize;

    use rostra_core::retention::RetentionPolicy;

    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let initial = post(remote, 1, "not a notification", None);
    ingest(&db, &initial).await?;
    let seen = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    let mut previous = initial.event_id();
    // Real edit ancestry must not become recursive read work for a notification.
    for n in 0..80 {
        let content = SocialPost::new_text(
            format!("edit {n} <rostra:{}>", local.id().to_short()),
            None,
            Default::default(),
        );
        let edit = signed(remote, &content, n + 2, Some(previous));
        ingest(&db, &edit).await?;
        previous = edit.event_id();
    }
    let one = NotificationBounds {
        rows: 1,
        raw_rows: 1,
        ..bounds()
    };
    let page = db.paginate_notifications(None, one).await?;
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].target.1, previous);
    assert_eq!(page.raw_rows_scanned, 1);
    assert_eq!(
        db.scan_notifications_unread(Some(seen), None, one)
            .await?
            .state,
        NotificationUnread::Yes
    );
    // Grow the real index independently of the page size. Membership checks
    // must continue to use one indexed key per candidate.
    let mut newest = None;
    for n in 0..300 {
        let p = post(
            remote,
            n + 100,
            format!("<rostra:{}> mention {n}", local.id().to_short()),
            None,
        );
        ingest(&db, &p).await?;
        newest = Some(p);
    }
    let page = db.paginate_notifications(None, one).await?;
    assert_eq!(page.raw_rows_scanned, 1);
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].reason.mention);
    let seen = page.highwater.unwrap();
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    let target = page.rows[0].target.1;
    let outcome = db
        .prune_quota_payload(crate::QuotaPruneRequest {
            id: target.to_short(),
            target: crate::QuotaPruneTarget::Processed,
            reason: crate::QuotaPruneReason::AuthorQuota,
            policy: RetentionPolicy::new(1, 1, 0, 0, 1, 10).unwrap(),
            clock: crate::RetentionClock::Trusted(Timestamp::from(1000)),
        })
        .await?;
    assert!(matches!(outcome, crate::QuotaPruneOutcome::Pruned { .. }));
    // Payload redelivery is not an implicit restoration of a pruned event.
    ingest(&db, &newest.unwrap()).await?;
    assert_ne!(
        db.paginate_notifications(None, one).await?.rows[0].target.1,
        target
    );
    let mut next = None;
    loop {
        let page = db
            .scan_notifications_unread(Some(seen.clone()), next, bounds())
            .await?;
        if page.next_cursor.is_none() {
            assert_eq!(page.state, NotificationUnread::No);
            break;
        }
        assert_eq!(page.state, NotificationUnread::Unknown);
        next = page.next_cursor;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn large_supported_payload_is_not_a_protocol_rejection_and_raw_length_is_checked()
-> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(local.id()).await?;
    let large = post(remote, 1, "x".repeat(1_100_000), Some(local.id()));
    ingest(&db, &large).await?;
    assert_eq!(
        db.paginate_notifications(None, bounds())
            .await?
            .issues
            .len(),
        1
    );
    let larger = NotificationBounds {
        payload_bytes: 2 * 1024 * 1024,
        ..bounds()
    };
    let page = db.paginate_notifications(None, larger).await?;
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].content.is_some());
    assert_eq!(page.payload_bytes_read, large.event.content_len() as usize);
    // A corrupt length prefix must not trick the reader into unbounded owned
    // decoding based on a small signed content length.
    db.write_with(|tx| {
        let mut store = tx
            .as_raw()
            .open_table(crate::content_store::TABLE.as_raw())?;
        let encoded_key =
            crate::payload_admission::encode_content_hash_key(large.event.event.content_hash);
        let encoded =
            bincode::encode_to_vec(vec![0u8; 2_200_000], redb_bincode::BINCODE_CONFIG).unwrap();
        store.insert(encoded_key.as_slice(), encoded.as_slice())?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.paginate_notifications(None, larger).await,
        Err(DbError::PayloadAccountingInvariant)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn version_37_upgrade_preserves_receipts_and_allocation_frontier() -> anyhow::Result<()> {
    let local = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("upgrade.redb");
    let db = Database::open(&path, local.id()).await?;
    ingest_at(&db, &post(remote, 1, "existing", Some(local.id())), 500).await?;
    let before = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    // Version 37 has the same receipt schema, without a notification epoch.
    db.write_with(|tx| {
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &37)?;
        tx.as_raw()
            .delete_table(crate::social_notification_epoch::TABLE.as_raw())?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Database::open(&path, local.id()).await?;
    let seen = db
        .paginate_notifications(None, bounds())
        .await?
        .highwater
        .unwrap();
    assert_eq!(seen.received_highwater(), before.received_highwater());
    assert_eq!(seen.next_sequence, before.next_sequence);
    ingest_at(&db, &post(remote, 2, "new backdated", Some(local.id())), 5).await?;
    assert_eq!(
        db.scan_notifications_unread(Some(seen), None, bounds())
            .await?
            .state,
        NotificationUnread::Yes
    );
    Ok(())
}
