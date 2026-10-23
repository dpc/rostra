use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;

use rostra_core::event::{
    Event, EventContentKind as _, EventContentRaw, EventExt as _, EventKind, PersonaTag,
    SocialPost, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, ExternalEventId, Timestamp};

use crate::social_post_mutation::{
    SocialPostMutation, SocialPostMutationRequest, SocialPostMutationRequestId,
};
use crate::{
    Database, DbError, DbResult, EventContentAvailability, PayloadAdmissionConfig,
    PayloadAdmissionLimits,
};

fn post(body: &str) -> SocialPost {
    SocialPost::new_text(body.to_owned(), None, BTreeSet::new())
}

fn signed(
    secret: RostraIdSecretKey,
    post: &SocialPost,
    replace: Option<EventId>,
    time: u64,
) -> VerifiedEventContent {
    raw_signed(secret, post.serialize_cbor().unwrap(), replace, time)
}

fn raw_signed(
    secret: RostraIdSecretKey,
    raw: EventContentRaw,
    replace: Option<EventId>,
    time: u64,
) -> VerifiedEventContent {
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(Timestamp::from(time).to_offset_date_time().unwrap())
        .maybe_delete(replace.map(|id| id.to_short()))
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

fn request(target: EventId, body: Option<&str>) -> SocialPostMutationRequest {
    SocialPostMutationRequest {
        expected_current: target,
        mutation: body.map_or(SocialPostMutation::Delete, |body| {
            SocialPostMutation::Edit {
                body: body.to_owned(),
            }
        }),
    }
}

#[test]
fn mutation_digest_v1() {
    let account = rostra_core::id::RostraId::from_bytes([3; 32]);
    let input = request(EventId::from_bytes([5; 32]), Some("hello\0世界\n"));
    let digest = input.digest(account);
    // Frozen V1 encoding, independent of future request/API additions.
    assert_eq!(
        digest,
        [
            53, 121, 4, 253, 221, 25, 51, 59, 34, 157, 115, 131, 17, 65, 14, 223, 35, 129, 83, 198,
            66, 209, 223, 240, 111, 61, 165, 117, 99, 178, 254, 27,
        ]
    );
    assert_ne!(
        digest,
        request(input.expected_current, None).digest(account)
    );
    assert_ne!(
        digest,
        request(input.expected_current, Some("hello\0世界\n ")).digest(account)
    );
    assert_ne!(
        digest,
        request(EventId::from_bytes([6; 32]), Some("hello\0世界\n")).digest(account)
    );
    assert_ne!(
        digest,
        input.digest(rostra_core::id::RostraId::from_bytes([4; 32]))
    );
}

async fn commit(
    db: &Database,
    id: u8,
    request: &SocialPostMutationRequest,
    candidate: &VerifiedEventContent,
) -> DbResult<VerifiedEvent> {
    db.commit_social_post_mutation(
        SocialPostMutationRequestId([id; 16]),
        request,
        candidate,
        &mut None,
        &mut None,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_metadata_lineage_delete_retry_and_prune() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let parent = signed(secret, &post("reply parent"), None, 1);
    ingest(&db, &parent).await?;
    // A historical persona and empty declared tags must not become effective tags.
    let mut original_post =
        SocialPost::decode_mutable_text(b"\xa3\x61p\x02\x61c\x61x\x61t\x80").unwrap();
    original_post.reply_to = Some(ExternalEventId::new(secret.id(), parent.event_id()));
    let original = signed(secret, &original_post, None, 2);
    ingest(&db, &original).await?;
    let reply = signed(
        secret,
        &SocialPost::new_text(
            "reply".into(),
            Some(ExternalEventId::new(secret.id(), original.event_id())),
            BTreeSet::from([PersonaTag::new("personal")?]),
        ),
        None,
        3,
    );
    ingest(&db, &reply).await?;
    let input = request(original.event_id(), Some(" ❤️ \n"));
    let mut edited_post = db.prepare_social_post_mutation(original.event_id()).await?;
    edited_post.djot_content = Some(" ❤️ \n".into());
    let edit = signed(secret, &edited_post, Some(original.event_id()), 4);
    let result = commit(&db, 1, &input, &edit).await?;
    let state = db
        .get_social_post_state(original.event_id().to_short())
        .await
        .unwrap();
    assert_eq!(state.event_id, result.event_id.to_short());
    assert!(!state.is_tombstone);
    let current = state.post.unwrap();
    assert_eq!(current.content, edited_post);
    assert_eq!(current.reply_count, 1);
    assert!(matches!(
        db.prepare_social_post_mutation(original.event_id()).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    let mut tombstone = edited_post.clone();
    tombstone.djot_content = Some(String::new());
    let deletion = signed(secret, &tombstone, Some(edit.event_id()), 5);
    let deletion_request = request(edit.event_id(), None);
    commit(&db, 2, &deletion_request, &deletion).await?;
    let state = db
        .get_social_post_state(original.event_id().to_short())
        .await
        .unwrap();
    assert_eq!(state.event_id, edit.event_id().to_short());
    assert_eq!(state.availability, EventContentAvailability::Deleted);
    assert!(!state.is_tombstone);
    assert!(state.post.is_none());
    assert!(
        db.get_social_post_state(deletion.event_id().to_short())
            .await
            .unwrap()
            .is_tombstone
    );
    assert!(matches!(
        db.prepare_social_post_mutation(edit.event_id()).await,
        Err(DbError::SocialPostMutationDeleted)
    ));
    assert_eq!(
        commit(&db, 1, &input, &edit).await?.event_id,
        edit.event_id()
    );
    assert_eq!(
        commit(&db, 2, &deletion_request, &deletion).await?.event_id,
        deletion.event_id()
    );
    assert!(matches!(
        commit(
            &db,
            1,
            &request(original.event_id(), Some("changed")),
            &edit
        )
        .await,
        Err(DbError::SocialPostMutationMismatch)
    ));
    // Prune the tombstone itself: receipt lookup must not need or restore bytes.
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
    commit(&db, 2, &deletion_request, &deletion).await?;
    assert!(matches!(
        db.get_event_content_availability(deletion.event_id()).await,
        Some(EventContentAvailability::Pruned { .. })
    ));
    assert_eq!(
        db.get_event_content_availability(edit.event_id()).await,
        Some(EventContentAvailability::Deleted)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_atomic_competing_edits_and_delete() -> anyhow::Result<()> {
    for delete in [false, true] {
        let secret = RostraIdSecretKey::generate();
        let db = Arc::new(Database::new_in_memory(secret.id()).await?);
        let original = signed(secret, &post("original"), None, 1);
        ingest(&db, &original).await?;
        let mut tasks = Vec::new();
        for index in 0..8u8 {
            let db = db.clone();
            let target = original.event_id();
            tasks.push(tokio::spawn(async move {
                let deleting = delete && index % 2 == 0;
                let body = format!("edit {index}");
                let input = request(target, (!deleting).then_some(body.as_str()));
                let candidate = signed(
                    secret,
                    &post(if deleting { "" } else { &body }),
                    Some(target),
                    u64::from(index) + 2,
                );
                let outcome = commit(&db, index, &input, &candidate).await;
                (outcome, candidate.event_id())
            }));
        }
        let mut winners = 0;
        for task in tasks {
            let (outcome, candidate) = task.await?;
            match outcome {
                Ok(_) => {
                    winners += 1;
                    assert!(db.get_event(candidate).await.is_some());
                }
                Err(DbError::SocialPostMutationConflict | DbError::SocialPostMutationDeleted) => {
                    assert!(db.get_event(candidate).await.is_none())
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(winners, 1);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_same_request_discards_distinct_signed_candidates() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    let original = signed(secret, &post("original"), None, 1);
    ingest(&db, &original).await?;
    let input = request(original.event_id(), Some("exact body"));
    let mut tasks = Vec::new();
    for timestamp in 2..10 {
        let db = db.clone();
        let input = input.clone();
        tasks.push(tokio::spawn(async move {
            let candidate = signed(
                secret,
                &post("exact body"),
                Some(input.expected_current),
                timestamp,
            );
            (
                commit(&db, 1, &input, &candidate).await,
                candidate.event_id(),
            )
        }));
    }
    let mut accepted = BTreeSet::new();
    let mut candidates = BTreeSet::new();
    for task in tasks {
        let (result, candidate) = task.await?;
        accepted.insert(result?.event_id);
        candidates.insert(candidate);
    }
    assert_eq!(accepted.len(), 1);
    assert_eq!(candidates.len(), 8);
    for candidate in candidates {
        assert_eq!(
            db.get_event(candidate).await.is_some(),
            accepted.contains(&candidate)
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_full_identity_ownership_types_and_remote_forks() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let original = signed(secret, &post("original"), None, 1);
    ingest(&db, &original).await?;
    let mut spoof = original.event_id().to_bytes();
    spoof[31] ^= 1;
    assert!(matches!(
        db.prepare_social_post_mutation(EventId::from_bytes(spoof))
            .await,
        Err(DbError::SocialPostMutationTargetMismatch)
    ));
    assert!(matches!(
        db.prepare_social_post_mutation(EventId::from_bytes([0; 32]))
            .await,
        Err(DbError::SocialPostMutationMissing)
    ));
    let other = signed(RostraIdSecretKey::generate(), &post("other"), None, 2);
    ingest(&db, &other).await?;
    assert!(matches!(
        db.prepare_social_post_mutation(other.event_id()).await,
        Err(DbError::SocialPostMutationNotOwner)
    ));
    for (index, unsupported) in [
        post("news").with_news_fields(None, None),
        SocialPost::new(
            "❤️".into(),
            Some(ExternalEventId::new(secret.id(), original.event_id())),
            BTreeSet::new(),
        ),
    ]
    .iter()
    .enumerate()
    {
        let content = signed(secret, unsupported, None, 20 + index as u64);
        ingest(&db, &content).await?;
        assert!(matches!(
            db.prepare_social_post_mutation(content.event_id()).await,
            Err(DbError::SocialPostMutationUnsupported)
        ));
    }
    // Remote wire ingestion remains permissive, but local mutation may not drop x.
    let unknown = raw_signed(
        secret,
        EventContentRaw::new(b"\xa2\x61c\x61x\x61z\x01".to_vec()),
        Some(original.event_id()),
        3,
    );
    ingest(&db, &unknown).await?;
    assert!(matches!(
        db.prepare_social_post_mutation(unknown.event_id()).await,
        Err(DbError::SocialPostMutationUnsupported)
    ));
    assert!(
        !db.get_social_post_state(unknown.event_id().to_short())
            .await
            .unwrap()
            .is_tombstone
    );
    // Deterministic fork selection uses lowest immediate short ID, not time.
    let fork = signed(secret, &post("fork"), Some(original.event_id()), 4);
    ingest(&db, &fork).await?;
    let (canonical, loser) = if fork.event_id().to_short() < unknown.event_id().to_short() {
        (&fork, &unknown)
    } else {
        (&unknown, &fork)
    };
    assert_eq!(
        db.get_social_post_state(original.event_id().to_short())
            .await
            .unwrap()
            .event_id,
        canonical.event_id().to_short()
    );
    assert!(matches!(
        db.prepare_social_post_mutation(loser.event_id()).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    assert!(matches!(
        db.prepare_social_post_mutation(original.event_id()).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    // A candidate prepared before remote ingestion cannot silently beat the new
    // snapshot.
    let stale = signed(
        secret,
        &post("stale prepared edit"),
        Some(original.event_id()),
        5,
    );
    assert!(matches!(
        commit(
            &db,
            1,
            &request(original.event_id(), Some("stale prepared edit")),
            &stale
        )
        .await,
        Err(DbError::SocialPostMutationConflict)
    ));
    assert!(db.get_event(stale.event_id()).await.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_rollback_admission_and_upgrade() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("upgrade.redb");
    let db = Database::open(&path, secret.id()).await?;
    // Real schema-34 table absence and version, with a retained create receipt.
    let create = crate::social_post_request::SocialPostRequest {
        body: "old create".into(),
        reply_to: None,
        persona_tags: BTreeSet::new(),
    };
    let original = signed(secret, &create.to_social_post(), None, 1);
    db.commit_social_post_request(
        crate::social_post_request::SocialPostRequestId([1; 16]),
        &create,
        &original,
        &mut None,
        &mut None,
    )
    .await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                "social_post_mutation_receipts",
            ))?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &34)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    let input = request(original.event_id(), None);
    let candidate = signed(secret, &post(""), Some(original.event_id()), 2);
    let result = db
        .commit_social_post_mutation_inner(
            SocialPostMutationRequestId([1; 16]),
            &input,
            &candidate,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::SocialPostCandidate))),
        )
        .await;
    assert!(matches!(result, Err(DbError::SocialPostCandidate)));
    assert!(db.get_event(candidate.event_id()).await.is_none());
    assert_eq!(
        db.get_event_content_availability(original.event_id()).await,
        Some(EventContentAvailability::Available)
    );
    assert!(
        db.social_post_mutation_receipt(SocialPostMutationRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
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
        commit(&db, 1, &input, &candidate).await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(db.get_event(candidate.event_id()).await.is_none());
    db.prepare_social_post_mutation(original.event_id()).await?;
    let config = PayloadAdmissionConfig::new(PayloadAdmissionLimits {
        database_bytes: NonZeroU64::new(10_000).unwrap(),
        author_bytes: NonZeroU64::new(10_000).unwrap(),
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
    for fail in [true, false] {
        let charged_candidate = candidate.clone();
        let mut allocation =
            db.reserve_payload_allocation(u64::from(charged_candidate.content_len()))?;
        let mut buffer = None;
        let outcome = db
            .commit_social_post_mutation_inner(
                SocialPostMutationRequestId([1; 16]),
                &input,
                &charged_candidate,
                &mut allocation,
                &mut buffer,
                fail.then(|| {
                    Box::new(|_: &crate::WriteTransactionCtx| Err(DbError::SocialPostCandidate))
                        as crate::social_post_receipt::ReceiptTestHook
                }),
            )
            .await;
        assert_eq!(outcome.is_err(), fail);
        assert!(buffer.is_some());
        drop(charged_candidate);
        drop(buffer);
        drop(allocation);
        assert_eq!(db.payload_admission_usage().acquisitions, 0);
        assert_eq!(db.payload_admission_usage().buffers, 0);
        assert_eq!(
            db.get_event_content_availability(original.event_id()).await,
            Some(if fail {
                EventContentAvailability::Available
            } else {
                EventContentAvailability::Deleted
            })
        );
    }
    db.write_with(|tx| Database::prepare_total_migration(tx, 35))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate).await?.event_id,
        candidate.event_id()
    );
    assert_eq!(
        db.social_post_receipt(
            crate::social_post_request::SocialPostRequestId([1; 16]),
            &create
        )
        .await?
        .unwrap()
        .event_id,
        original.event_id()
    );
    assert_eq!(
        db.get_event_content_availability(original.event_id()).await,
        Some(EventContentAvailability::Deleted)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_commit_before_ack_cancellation_reopen() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cancel.redb");
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    let original = signed(secret, &post("original"), None, 1);
    ingest(&db, &original).await?;
    let input = request(original.event_id(), Some("edited"));
    let edit = signed(secret, &post("edited"), Some(original.event_id()), 2);
    let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task_db = db.clone();
    let task_input = input.clone();
    let task_edit = edit.clone();
    let task = tokio::spawn(async move {
        task_db
            .commit_social_post_mutation_inner(
                SocialPostMutationRequestId([1; 16]),
                &task_input,
                &task_edit,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(move || {
                        committed_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        panic!("acknowledgment lost after durable commit");
                    });
                    Ok(())
                })),
            )
            .await
    });
    committed_rx.await?;
    task.abort();
    release_tx.send(())?;
    let _ = task.await;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &edit).await?.event_id,
        edit.event_id()
    );
    assert_eq!(
        db.get_social_post_state(original.event_id().to_short())
            .await
            .unwrap()
            .event_id,
        edit.event_id().to_short()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_canonical_fork_descendants_and_unavailable_classification() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let original = signed(secret, &post("root"), None, 1);
    // An envelope alone cannot authorize a metadata-losing edit.
    db.try_process_event(&original.event).await?;
    assert!(matches!(
        db.prepare_social_post_mutation(original.event_id()).await,
        Err(DbError::SocialPostMutationUnavailable)
    ));
    ingest(&db, &original).await?;
    let a = signed(secret, &post("branch a"), Some(original.event_id()), 2);
    let b = signed(secret, &post("branch b"), Some(original.event_id()), 3);
    ingest(&db, &a).await?;
    ingest(&db, &b).await?;
    let aa = signed(secret, &post("leaf a"), Some(a.event_id()), 4);
    let bb = signed(secret, &post("leaf b"), Some(b.event_id()), 5);
    ingest(&db, &bb).await?;
    ingest(&db, &aa).await?;
    let (winner, loser) = if a.event_id().to_short() < b.event_id().to_short() {
        (&aa, &bb)
    } else {
        (&bb, &aa)
    };
    assert_eq!(
        db.get_social_post_state(original.event_id().to_short())
            .await
            .unwrap()
            .event_id,
        winner.event_id().to_short()
    );
    db.prepare_social_post_mutation(winner.event_id()).await?;
    let replay = Database::new_in_memory(secret.id()).await?;
    // Remote replay may deliver leaves before their deleted parents' payloads.
    for content in [&bb, &aa, &b, &a, &original] {
        ingest(&replay, content).await?;
    }
    assert_eq!(
        replay
            .get_social_post_state(original.event_id().to_short())
            .await
            .unwrap()
            .event_id,
        winner.event_id().to_short()
    );
    replay
        .prepare_social_post_mutation(winner.event_id())
        .await?;
    assert!(matches!(
        db.prepare_social_post_mutation(loser.event_id()).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    let input = request(winner.event_id(), Some("accepted"));
    // Even a trusted publisher cannot accidentally drop/change exact edit text.
    let wrong = signed(secret, &post("not requested"), Some(winner.event_id()), 6);
    assert!(matches!(
        commit(&db, 1, &input, &wrong).await,
        Err(DbError::SocialPostCandidate)
    ));
    assert!(db.get_event(wrong.event_id()).await.is_none());
    db.prepare_social_post_mutation(winner.event_id()).await?;
    let good = signed(secret, &post("accepted"), Some(winner.event_id()), 7);
    commit(&db, 1, &input, &good).await?;
    assert!(matches!(
        commit(&db, 2, &request(good.event_id(), Some(" \n")), &good).await,
        Err(DbError::SocialPostMutationEmpty)
    ));
    // A valid remote reaction-shaped deleting payload has no ordinary post
    // projection; that absence must not imply a supported text tombstone.
    let unsupported = signed(
        secret,
        &SocialPost::new(
            "❤️".into(),
            Some(ExternalEventId::new(secret.id(), original.event_id())),
            BTreeSet::new(),
        ),
        Some(good.event_id()),
        8,
    );
    ingest(&db, &unsupported).await?;
    let state = db
        .get_social_post_state(unsupported.event_id().to_short())
        .await
        .unwrap();
    assert!(state.post.is_none());
    assert_eq!(state.availability, EventContentAvailability::Available);
    assert!(!state.is_tombstone);
    assert!(matches!(
        db.prepare_social_post_mutation(unsupported.event_id())
            .await,
        Err(DbError::SocialPostMutationUnsupported)
    ));
    Ok(())
}
