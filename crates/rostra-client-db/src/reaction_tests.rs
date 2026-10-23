use std::collections::BTreeSet;

use rostra_core::event::{
    Event, EventContentKind as _, EventExt as _, EventKind, SocialPost, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, ExternalEventId, Timestamp};

use crate::reaction_request::{ReactionOperation, ReactionRequest, ReactionRequestId};
use crate::{Database, DbError, DbResult};

fn signed(
    secret: RostraIdSecretKey,
    post: &SocialPost,
    remove: Option<EventId>,
    time: u64,
) -> VerifiedEventContent {
    let raw = post.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(Timestamp::from(time).to_offset_date_time().unwrap())
        .maybe_delete(remove.map(|id| id.to_short()))
        .content(&raw)
        .build()
        .signed_by(secret);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event).unwrap(),
        raw,
    )
    .unwrap()
}

async fn ingest(db: &Database, content: &VerifiedEventContent) -> DbResult<()> {
    db.write_with(|tx| {
        db.process_event_tx(&content.event, Timestamp::from(100), tx)?;
        db.process_event_content_tx(content, Timestamp::from(100), tx)
    })
    .await
}

fn add(target: &VerifiedEventContent) -> ReactionRequest {
    ReactionRequest {
        target: (target.author(), target.event_id()),
        operation: ReactionOperation::Add {
            reaction: "❤️".into(),
        },
    }
}

fn remove(target: &VerifiedEventContent, row: EventId) -> ReactionRequest {
    ReactionRequest {
        target: (target.author(), target.event_id()),
        operation: ReactionOperation::Remove {
            expected_current: row,
        },
    }
}

async fn candidate(
    db: &Database,
    secret: RostraIdSecretKey,
    id: u8,
    request: &ReactionRequest,
    time: u64,
) -> DbResult<VerifiedEventContent> {
    let mut content = signed(
        secret,
        &db.prepare_reaction(request).await?,
        request.expected_current(),
        time,
    );
    let mut event = content.event.event;
    event.key_aux = ReactionRequestId([id; 16]).event_aux_key(secret.id());
    content.event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap();
    Ok(content)
}

async fn commit(
    db: &Database,
    id: u8,
    request: &ReactionRequest,
    candidate: &VerifiedEventContent,
) -> DbResult<VerifiedEvent> {
    db.commit_reaction(
        ReactionRequestId([id; 16]),
        request,
        candidate,
        &mut None,
        &mut None,
    )
    .await
}

fn text(body: &str) -> SocialPost {
    SocialPost::new_text(body.into(), None, BTreeSet::new())
}

#[test]
fn reaction_digest_v1() {
    let account = rostra_core::id::RostraId::from_bytes([3; 32]);
    let request = ReactionRequest {
        target: (
            rostra_core::id::RostraId::from_bytes([4; 32]),
            EventId::from_bytes([5; 32]),
        ),
        operation: ReactionOperation::Add {
            reaction: "❤️".into(),
        },
    };
    assert_eq!(
        request.digest(account),
        [
            113, 219, 1, 154, 234, 171, 49, 202, 38, 0, 28, 160, 31, 6, 76, 162, 218, 110, 166,
            232, 0, 198, 229, 4, 217, 223, 152, 153, 183, 191, 230, 100,
        ]
    );
    let request = ReactionRequest {
        operation: ReactionOperation::Remove {
            expected_current: EventId::from_bytes([6; 32]),
        },
        ..request
    };
    assert_eq!(
        request.digest(account),
        [
            176, 249, 26, 198, 43, 202, 70, 26, 0, 234, 141, 227, 67, 131, 143, 17, 153, 94, 2,
            137, 249, 230, 87, 51, 156, 82, 66, 91, 253, 89, 106, 146,
        ]
    );
    assert_eq!(
        ReactionRequestId([7; 16]).event_aux_key(account).to_bytes(),
        [
            233, 23, 131, 207, 110, 161, 133, 174, 73, 207, 52, 99, 63, 134, 106, 130
        ],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn distinct_request_ids_same_head_and_second_remain_independent() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(secret, &text("target"), None, 1);
    ingest(&db, &target).await?;
    let input = add(&target);
    let a = candidate(&db, secret, 1, &input, 2).await?;
    let b = candidate(&db, secret, 2, &input, 2).await?;
    assert_eq!(a.timestamp(), b.timestamp());
    assert_eq!(a.parent_prev(), b.parent_prev());
    assert_eq!(a.content_hash(), b.content_hash());
    assert_ne!(a.event_id(), b.event_id());
    assert!(!a.is_singleton());
    assert!(!b.is_singleton());
    let (left, right) = tokio::join!(commit(&db, 1, &input, &a), commit(&db, 2, &input, &b));
    left?;
    right?;
    assert_eq!(db.reaction_summary(input.target, None, 10).await?.total, 2);
    assert!(matches!(
        commit(&db, 3, &input, &a).await,
        Err(DbError::SocialPostCandidate)
    ));
    assert!(
        db.reaction_receipt(ReactionRequestId([3; 16]), &input)
            .await?
            .is_none()
    );
    // Existing unchanged ingress/projection accepts nonzero nonsingleton keys.
    let replica = Database::new_in_memory(secret.id()).await?;
    for event in [&b, &a, &target] {
        ingest(&replica, event).await?;
    }
    assert_eq!(
        replica
            .reaction_summary(input.target, None, 10)
            .await?
            .total,
        2
    );
    // No receipt exists on a replica: it must not accept an already-known event
    // as a new independent local intent even with the expected nonce.
    assert!(matches!(
        commit(&replica, 1, &input, &a).await,
        Err(DbError::ReactionConflict)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_adds_races_exact_remove_and_receipt_after_deletion() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(secret, &text("post"), None, 1);
    ingest(&db, &target).await?;
    let input = add(&target);
    let a = candidate(&db, secret, 1, &input, 2).await?;
    let b = candidate(&db, secret, 1, &input, 3).await?;
    let (first, retry) = tokio::join!(commit(&db, 1, &input, &a), commit(&db, 1, &input, &b));
    let first = first?;
    assert_eq!(first.event_id, retry?.event_id);
    let losing = if first.event_id == a.event_id() {
        &b
    } else {
        &a
    };
    assert!(db.get_event(losing.event_id()).await.is_none());
    let independent = candidate(&db, secret, 2, &input, 4).await?;
    commit(&db, 2, &input, &independent).await?;
    let summary = db.reaction_summary(input.target, None, 1).await?;
    assert_eq!(
        (summary.total, summary.self_count, summary.rows.len()),
        (2, 2, 1)
    );
    let page2 = db
        .reaction_summary(input.target, summary.next_cursor, 1)
        .await?;
    assert_eq!((page2.total, page2.self_count, page2.rows.len()), (2, 2, 1));
    assert_ne!(summary.rows[0].cursor, page2.rows[0].cursor);
    assert!(page2.next_cursor.is_none());

    let removal = remove(&target, first.event_id);
    let c = candidate(&db, secret, 3, &removal, 5).await?;
    let d = candidate(&db, secret, 4, &removal, 6).await?;
    let (left, right) = tokio::join!(commit(&db, 3, &removal, &c), commit(&db, 4, &removal, &d));
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let (key, accepted) = if let Ok(event) = left {
        (3, event)
    } else {
        (4, right?)
    };
    assert_eq!(
        commit(&db, key, &removal, &c).await?.event_id,
        accepted.event_id
    );
    assert_eq!(commit(&db, 1, &input, &a).await?.event_id, first.event_id);
    assert_eq!(db.reaction_summary(input.target, None, 10).await?.total, 1);
    let changed = ReactionRequest {
        operation: ReactionOperation::Add {
            reaction: "👍".into(),
        },
        ..input.clone()
    };
    assert!(matches!(
        commit(&db, 1, &changed, &a).await,
        Err(DbError::ReactionMismatch)
    ));

    let deleted_target = signed(secret, &text(""), Some(target.event_id()), 7);
    ingest(&db, &deleted_target).await?;
    assert!(matches!(
        db.prepare_reaction(&input).await,
        Err(DbError::ReactionUnavailable)
    ));
    let removal = remove(&target, independent.event_id());
    let c = candidate(&db, secret, 5, &removal, 8).await?;
    commit(&db, 5, &removal, &c).await?;
    assert_eq!(db.reaction_summary(input.target, None, 10).await?.total, 0);
    assert_eq!(
        commit(&db, 2, &input, &independent).await?.event_id,
        independent.event_id()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn historical_targets_exact_bindings_and_unsupported_inputs() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(other, &text("original"), None, 1);
    ingest(&db, &target).await?;
    let input = add(&target);
    let row = candidate(&db, secret, 1, &input, 2).await?;
    commit(&db, 1, &input, &row).await?;
    let edited = signed(other, &text("edited"), Some(target.event_id()), 3);
    ingest(&db, &edited).await?;
    assert!(matches!(
        db.prepare_reaction(&input).await,
        Err(DbError::ReactionConflict)
    ));
    let new_input = add(&edited);
    let second = candidate(&db, secret, 2, &new_input, 4).await?;
    commit(&db, 2, &new_input, &second).await?;
    for target in [input.target, new_input.target] {
        let summary = db.reaction_summary(target, None, 100).await?;
        assert_eq!((summary.total, summary.self_count), (2, 2));
    }
    let removal = remove(&edited, row.event_id());
    let c = candidate(&db, secret, 3, &removal, 5).await?;
    commit(&db, 3, &removal, &c).await?;
    assert_eq!(
        db.reaction_summary(new_input.target, None, 10).await?.total,
        1
    );
    let mut spoof = new_input.clone();
    spoof.target.0 = secret.id();
    assert!(matches!(
        db.prepare_reaction(&spoof).await,
        Err(DbError::ReactionTarget)
    ));
    let mut bytes = new_input.target.1.to_bytes();
    bytes[31] ^= 1;
    spoof = new_input.clone();
    spoof.target.1 = EventId::from_bytes(bytes);
    assert!(matches!(
        db.prepare_reaction(&spoof).await,
        Err(DbError::ReactionTarget)
    ));
    assert!(matches!(
        db.reaction_summary(spoof.target, None, 10).await,
        Err(DbError::ReactionTarget)
    ));
    for reaction in ["", " ", "a", "👍👍", " 👍", "👍 "] {
        let bad = ReactionRequest {
            operation: ReactionOperation::Add {
                reaction: reaction.into(),
            },
            ..new_input.clone()
        };
        assert!(matches!(
            db.prepare_reaction(&bad).await,
            Err(DbError::ReactionInvalidInput)
        ));
    }
    let change = ReactionRequest {
        operation: ReactionOperation::Change {
            expected_current: second.event_id(),
            reaction: "👍".into(),
        },
        ..new_input.clone()
    };
    assert!(matches!(
        commit(&db, 9, &change, &second).await,
        Err(DbError::ReactionUnsupported)
    ));
    assert!(
        db.reaction_receipt(ReactionRequestId([9; 16]), &change)
            .await?
            .is_none()
    );
    let third = candidate(&db, secret, 9, &new_input, 8).await?;
    commit(&db, 9, &new_input, &third).await?;
    let news = signed(other, &text("news").with_news_fields(None, None), None, 9);
    ingest(&db, &news).await?;
    assert!(matches!(
        db.prepare_reaction(&add(&news)).await,
        Err(DbError::ReactionUnsupported)
    ));
    assert!(matches!(
        db.prepare_reaction(&add(&second)).await,
        Err(DbError::ReactionUnsupported)
    ));
    let foreign = signed(
        other,
        &SocialPost::new_reaction(
            "👍".into(),
            ExternalEventId::new(other.id(), edited.event_id()),
        )
        .unwrap(),
        None,
        10,
    );
    ingest(&db, &foreign).await?;
    assert!(matches!(
        db.prepare_reaction(&remove(&edited, foreign.event_id()))
            .await,
        Err(DbError::ReactionNotOwner)
    ));
    let unrelated = signed(other, &text("other"), None, 11);
    ingest(&db, &unrelated).await?;
    assert!(matches!(
        db.prepare_reaction(&remove(&unrelated, second.event_id()))
            .await,
        Err(DbError::ReactionTarget)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn receipt_reopen_incremental_upgrade_and_total_rebuild() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let secret = RostraIdSecretKey::generate();
    let db = Database::open(&path, secret.id()).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                "social_reaction_receipts",
            ))?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &35)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    let target = signed(secret, &text("target"), None, 1);
    ingest(&db, &target).await?;
    let input = add(&target);
    let row = candidate(&db, secret, 1, &input, 2).await?;
    commit(&db, 1, &input, &row).await?;
    let removal = remove(&target, row.event_id());
    let deletion = candidate(&db, secret, 2, &removal, 3).await?;
    commit(&db, 2, &removal, &deletion).await?;
    let retained = candidate(&db, secret, 3, &input, 4).await?;
    commit(&db, 3, &input, &retained).await?;
    db.write_with(|tx| {
        let before = db.payload_before_tx(tx, &deletion.event)?;
        Database::prune_event_content_tx(
            deletion.event_id(),
            deletion.content_hash(),
            &mut tx.open_table(&crate::events_content_state::TABLE)?,
            &mut tx.open_table(&crate::content_rc::TABLE)?,
            &mut tx.open_table(&crate::events_content_missing::TABLE)?,
            Some((
                secret.id(),
                deletion.content_len(),
                &mut tx.open_table(&crate::ids_data_usage::TABLE)?,
            )),
        )?;
        db.payload_after_tx(tx, before)
    })
    .await?;
    assert!(db.get_event_content(deletion.event_id()).await.is_none());
    commit(&db, 2, &removal, &deletion).await?;
    assert!(db.get_event_content(deletion.event_id()).await.is_none());
    db.write_with(|tx| Database::prepare_total_migration(tx, 36))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    let before_retry = db.get_event_content_availability(deletion.event_id()).await;
    assert_eq!(commit(&db, 1, &input, &row).await?.event_id, row.event_id());
    assert_eq!(
        commit(&db, 2, &removal, &deletion).await?.event_id,
        deletion.event_id()
    );
    assert_eq!(db.reaction_summary(input.target, None, 10).await?.total, 1);
    assert!(db.get_event_content(row.event_id()).await.is_none());
    assert_eq!(
        db.get_event_content_availability(deletion.event_id()).await,
        before_retry
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn summary_limits_raw_rows_and_reaction_payload_type_policy() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(secret, &text("target"), None, 1);
    ingest(&db, &target).await?;
    let input = add(&target);
    let row = candidate(&db, secret, 1, &input, 2).await?;
    commit(&db, 1, &input, &row).await?;
    let mut unsupported = SocialPost::new_reaction(
        "👍".into(),
        ExternalEventId::new(secret.id(), target.event_id()),
    )
    .unwrap();
    unsupported.djot_content = Some("mixed".into());
    let mixed = signed(secret, &unsupported, None, 3);
    ingest(&db, &mixed).await?;
    let summary = db.reaction_summary(input.target, None, 10).await?;
    assert_eq!((summary.total, summary.raw_rows_scanned), (1, 2));
    assert!(matches!(
        db.prepare_reaction(&remove(&target, mixed.event_id()))
            .await,
        Err(DbError::ReactionUnsupported)
    ));
    for limit in [0, 101] {
        assert!(matches!(
            db.reaction_summary(input.target, None, limit).await,
            Err(DbError::ReactionPageLimit)
        ));
    }
    db.write_with(|tx| {
        let mut index = tx.open_table(&crate::social_posts_reactions::TABLE)?;
        for time in 10..10008 {
            index.insert(
                &(
                    target.event_id().to_short(),
                    Timestamp::from(time),
                    row.event_id().to_short(),
                ),
                &crate::SocialPostsReactionsRecord,
            )?;
        }
        Ok(())
    })
    .await?;
    let boundary = db.reaction_summary(input.target, None, 10).await?;
    assert_eq!((boundary.total, boundary.raw_rows_scanned), (1, 10_000));
    db.write_with(|tx| {
        tx.open_table(&crate::social_posts_reactions::TABLE)?
            .insert(
                &(
                    target.event_id().to_short(),
                    Timestamp::from(10008),
                    row.event_id().to_short(),
                ),
                &crate::SocialPostsReactionsRecord,
            )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.reaction_summary(input.target, None, 10).await,
        Err(DbError::ReactionScanLimit)
    ));
    // Invalid/cyclic lineage must fail instead of looping or yielding partial
    // counts.
    db.write_with(|tx| {
        tx.open_table(&crate::social_posts_replaces::TABLE)?
            .insert(
                &(
                    secret.id(),
                    target.event_id().to_short(),
                    target.event_id().to_short(),
                ),
                &(),
            )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.reaction_summary(input.target, None, 10).await,
        Err(DbError::ReactionHistoryLimit)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_rollback_preserves_row_and_rejects_wrong_candidate() -> anyhow::Result<()> {
    use std::num::{NonZeroU64, NonZeroUsize};

    use crate::{EventContentAvailability, PayloadAdmissionConfig, PayloadAdmissionLimits};

    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(secret, &text("target"), None, 1);
    ingest(&db, &target).await?;
    let input = add(&target);
    let row = candidate(&db, secret, 1, &input, 2).await?;
    let mut wrong = signed(secret, &text("not a reaction"), None, 3);
    let mut wrong_event = wrong.event.event;
    wrong_event.key_aux = ReactionRequestId([1; 16]).event_aux_key(secret.id());
    wrong.event = VerifiedEvent::verify_signed(secret.id(), wrong_event.signed_by(secret)).unwrap();
    assert!(matches!(
        commit(&db, 1, &input, &wrong).await,
        Err(DbError::SocialPostCandidate)
    ));
    assert!(db.get_event(wrong.event_id()).await.is_none());
    commit(&db, 1, &input, &row).await?;
    let removal = remove(&target, row.event_id());
    let deletion = candidate(&db, secret, 2, &removal, 4).await?;
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
        in_flight_bytes: NonZeroU64::new(10_000).unwrap(),
    })
    .unwrap();
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(config);
        Ok(())
    })
    .await?;
    assert!(matches!(
        commit(&db, 2, &removal, &deletion).await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(db.get_event(deletion.event_id()).await.is_none());
    assert_eq!(
        db.get_event_content_availability(row.event_id()).await,
        Some(EventContentAvailability::Available)
    );
    assert_eq!(db.reaction_summary(input.target, None, 10).await?.total, 1);
    assert!(
        db.reaction_receipt(ReactionRequestId([2; 16]), &removal)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn reverse_replay_preserves_deletion_and_does_not_reinterpret_change() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let target = signed(secret, &text("target"), None, 1);
    let reaction = SocialPost::new_reaction(
        "❤️".into(),
        ExternalEventId::new(secret.id(), target.event_id()),
    )
    .unwrap();
    let row = signed(secret, &reaction, None, 2);
    // This historical shape remains Skip; it is NOT a supported Change.
    let ambiguous = signed(secret, &reaction, Some(row.event_id()), 3);
    for reverse in [false, true] {
        let db = Database::new_in_memory(secret.id()).await?;
        let mut events = vec![&target, &row, &ambiguous];
        if reverse {
            events.reverse();
        }
        for event in events {
            ingest(&db, event).await?;
        }
        let summary = db
            .reaction_summary((secret.id(), target.event_id()), None, 10)
            .await?;
        assert_eq!(
            (summary.total, summary.self_count, summary.raw_rows_scanned),
            (0, 0, 0)
        );
        let count = db
            .read_with(|tx| {
                Ok(tx
                    .open_table(&crate::social_posts::TABLE)?
                    .get(&target.event_id().to_short())?
                    .map(|row| row.value().reaction_count)
                    .unwrap_or_default())
            })
            .await?;
        assert_eq!(count, 0);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn summary_rejects_excess_cumulative_decoding_even_for_unsupported_rows() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(secret, &text("target"), None, 1);
    ingest(&db, &target).await?;
    let mut mixed = SocialPost::new_reaction(
        "❤️".into(),
        ExternalEventId::new(secret.id(), target.event_id()),
    )
    .unwrap();
    mixed.djot_content = Some("x".repeat(900_000));
    for time in 2..21 {
        ingest(&db, &signed(secret, &mixed, None, time)).await?;
    }
    assert!(matches!(
        db.reaction_summary((secret.id(), target.event_id()), None, 1)
            .await,
        Err(DbError::ReactionPayloadLimit)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn summary_counts_more_than_one_thousand_materialized_rows_before_pagination()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = signed(secret, &text("target"), None, 1);
    ingest(&db, &target).await?;
    let post = SocialPost::new_reaction(
        "❤️".into(),
        ExternalEventId::new(secret.id(), target.event_id()),
    )
    .unwrap();
    db.write_with(|tx| {
        for time in 2..1003 {
            let row = signed(
                if time % 2 == 0 { secret } else { other },
                &post,
                None,
                time,
            );
            db.process_event_tx(&row.event, Timestamp::from(2000), tx)?;
            db.process_event_content_tx(&row, Timestamp::from(2000), tx)?;
        }
        Ok(())
    })
    .await?;
    let summary = db
        .reaction_summary((secret.id(), target.event_id()), None, 100)
        .await?;
    assert_eq!(
        (summary.total, summary.self_count, summary.rows.len()),
        (1001, 501, 100)
    );
    let next = db
        .reaction_summary((secret.id(), target.event_id()), summary.next_cursor, 100)
        .await?;
    assert_eq!(
        (next.total, next.self_count, next.rows.len()),
        (1001, 501, 100)
    );
    assert!(
        next.rows
            .iter()
            .all(|row| row.cursor < summary.rows.last().unwrap().cursor)
    );
    Ok(())
}
