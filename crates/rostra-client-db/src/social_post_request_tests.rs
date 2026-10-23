use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;

use rostra_core::event::{
    Event, EventContentKind as _, EventExt as _, EventKind, PersonaTag, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, Timestamp};

use crate::social_post_request::{SocialPostRequest, SocialPostRequestId};
use crate::{Database, DbError, PayloadAdmissionConfig, PayloadAdmissionLimits};

fn request(body: &str) -> SocialPostRequest {
    SocialPostRequest {
        body: body.to_owned(),
        reply_to: None,
        persona_tags: BTreeSet::new(),
    }
}

fn candidate(
    secret: RostraIdSecretKey,
    request: &SocialPostRequest,
    time: u64,
) -> VerifiedEventContent {
    let raw = request.to_social_post().serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(Timestamp::from(time).to_offset_date_time().unwrap())
        .content(&raw)
        .build()
        .signed_by(secret);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event).unwrap(),
        raw,
    )
    .unwrap()
}

async fn commit(
    db: &Database,
    id: u8,
    request: &SocialPostRequest,
    candidate: &VerifiedEventContent,
) -> crate::DbResult<VerifiedEvent> {
    db.commit_social_post_request(
        SocialPostRequestId([id; 16]),
        request,
        candidate,
        &mut None,
        &mut None,
    )
    .await
}

#[test]
fn social_post_request_canonical_v1() {
    let account = RostraId::from_bytes([3; 32]);
    let mut input = request("hello\0世界\n");
    input.reply_to = Some((RostraId::from_bytes([4; 32]), EventId::from_bytes([5; 32])));
    input.persona_tags =
        BTreeSet::from([PersonaTag::new("z").unwrap(), PersonaTag::new("a").unwrap()]);
    let digest = input.digest(account);
    // Frozen interoperability vector for the exact versioned byte encoding.
    assert_eq!(
        digest,
        [
            182, 131, 170, 217, 202, 218, 93, 179, 38, 28, 128, 139, 186, 166, 53, 111, 227, 148,
            236, 3, 18, 170, 85, 157, 140, 184, 188, 184, 187, 160, 229, 220
        ]
    );
    let mut changed = input.clone();
    changed.body.push(' ');
    assert_ne!(digest, changed.digest(account));
    changed = input.clone();
    changed.reply_to.as_mut().unwrap().1 = EventId::from_bytes([6; 32]);
    assert_ne!(digest, changed.digest(account));
    changed = input.clone();
    changed.reply_to = None;
    assert_ne!(digest, changed.digest(account));
    changed = input.clone();
    changed.persona_tags.insert(PersonaTag::new("b").unwrap());
    assert_ne!(digest, changed.digest(account));
    assert_ne!(digest, input.digest(RostraId::from_bytes([7; 32])));
}

#[tokio::test(flavor = "multi_thread")]
async fn social_post_request_races_mismatch_and_full_target() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    let input = request("same input");
    let mut tasks = Vec::new();
    for time in 1..=8 {
        let db = db.clone();
        let input = input.clone();
        tasks.push(tokio::spawn(async move {
            commit(&db, 1, &input, &candidate(secret, &input, time)).await
        }));
    }
    let mut ids = BTreeSet::new();
    for task in tasks {
        ids.insert(task.await??.event_id);
    }
    assert_eq!(ids.len(), 1);
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&crate::events_self::TABLE)?
                .range(..)?
                .count(),
            1
        );
        Ok(())
    })
    .await?;
    let changed = request("changed input");
    assert!(matches!(
        commit(&db, 1, &changed, &candidate(secret, &changed, 20)).await,
        Err(DbError::SocialPostRequestMismatch)
    ));
    let mut reply = request("❤️");
    let target = *ids.first().unwrap();
    reply.reply_to = Some((secret.id(), target));
    let posted = commit(&db, 2, &reply, &candidate(secret, &reply, 21)).await?;
    assert_eq!(
        db.get_social_post(posted.event_id.to_short())
            .await
            .unwrap()
            .content
            .djot_content
            .as_deref(),
        Some("❤️")
    );
    let mut wrong = reply.clone();
    let mut bytes = target.to_bytes();
    bytes[31] ^= 1;
    wrong.reply_to.as_mut().unwrap().1 = EventId::from_bytes(bytes);
    assert_eq!(target.to_short(), wrong.reply_to.unwrap().1.to_short());
    assert!(matches!(
        commit(&db, 2, &wrong, &candidate(secret, &wrong, 22)).await,
        Err(DbError::SocialPostRequestMismatch)
    ));
    assert!(matches!(
        commit(&db, 3, &wrong, &candidate(secret, &wrong, 22)).await,
        Err(DbError::SocialPostReplyTarget)
    ));
    wrong = reply.clone();
    wrong.reply_to.as_mut().unwrap().0 = RostraIdSecretKey::generate().id();
    assert!(matches!(
        commit(&db, 3, &wrong, &candidate(secret, &wrong, 22)).await,
        Err(DbError::SocialPostReplyTarget)
    ));
    let mut changed_tags = input.clone();
    changed_tags
        .persona_tags
        .insert(PersonaTag::new("other").unwrap());
    assert!(matches!(
        commit(&db, 1, &changed_tags, &candidate(secret, &changed_tags, 23)).await,
        Err(DbError::SocialPostRequestMismatch)
    ));
    let deletion = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .delete(target.to_short())
        .build()
        .signed_by(secret);
    db.try_process_event(&VerifiedEvent::verify_signed(secret.id(), deletion)?)
        .await?;
    assert_eq!(
        commit(&db, 2, &reply, &candidate(secret, &reply, 24))
            .await?
            .event_id,
        posted.event_id
    );
    // Prune the reply through the same lifecycle primitive used by retention.
    db.write_with(|tx| {
        let before = db.payload_before_tx(tx, &posted)?;
        Database::prune_event_content_tx(
            posted.event_id,
            posted.content_hash(),
            &mut tx.open_table(&crate::events_content_state::TABLE)?,
            &mut tx.open_table(&crate::content_rc::TABLE)?,
            &mut tx.open_table(&crate::events_content_missing::TABLE)?,
            Some((
                posted.author(),
                posted.content_len(),
                &mut tx.open_table(&crate::ids_data_usage::TABLE)?,
            )),
        )?;
        db.payload_after_tx(tx, before)
    })
    .await?;
    assert_eq!(
        commit(&db, 2, &reply, &candidate(secret, &reply, 25))
            .await?
            .event_id,
        posted.event_id
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn social_post_request_commit_before_ack_reopen_delete_and_migrate() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("account.redb");
    let input = request("durable original");
    let content = candidate(secret, &input, 1);
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    // A real post-commit hook panic loses the response after durable commit.
    let task_db = db.clone();
    let task_input = input.clone();
    let task_content = content.clone();
    let failed = tokio::spawn(async move {
        task_db
            .commit_social_post_request_inner(
                SocialPostRequestId([1; 16]),
                &task_input,
                &task_content,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("lost acknowledgment"));
                    Ok(())
                })),
            )
            .await
    })
    .await;
    assert!(failed.unwrap_err().is_panic());
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate(secret, &input, 2))
            .await?
            .event_id,
        content.event_id()
    );
    let deletion = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .delete(content.event_id().to_short())
        .build()
        .signed_by(secret);
    db.try_process_event(&VerifiedEvent::verify_signed(secret.id(), deletion)?)
        .await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate(secret, &input, 3))
            .await?
            .event_id,
        content.event_id()
    );
    // Commit the actual current-schema stash, then reopen to finish recovery.
    db.write_with(|tx| Database::prepare_total_migration(tx, 34))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate(secret, &input, 4))
            .await?
            .event_id,
        content.event_id()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn social_post_request_rollback_upgrade_and_admission() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("account.redb");
    let db = Database::open(&path, secret.id()).await?;
    // Model a v33 database: no receipt table and its actual previous version.
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                "social_post_receipts",
            ))?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &33)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    let input = request("rollback");
    let content = candidate(secret, &input, 1);
    let error = db
        .commit_social_post_request_inner(
            SocialPostRequestId([1; 16]),
            &input,
            &content,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::SocialPostCandidate))),
        )
        .await;
    assert!(matches!(error, Err(DbError::SocialPostCandidate)));
    assert!(
        db.social_post_receipt(SocialPostRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
    assert!(db.get_event(content.event_id()).await.is_none());
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
    let mut allocation = db.reserve_payload_allocation(u64::from(content.content_len()))?;
    let mut buffer = None;
    assert!(matches!(
        db.commit_social_post_request(
            SocialPostRequestId([1; 16]),
            &input,
            &content,
            &mut allocation,
            &mut buffer
        )
        .await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    drop(content);
    drop(buffer);
    drop(allocation);
    assert!(
        db.social_post_receipt(SocialPostRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(db.payload_admission_usage().buffers, 0);
    db.read_with(|tx| {
        assert_eq!(
            tx.open_table(&crate::events_self::TABLE)?
                .range(..)?
                .count(),
            0
        );
        Ok(())
    })
    .await?;
    // Reconfigure this private fixture only: production startup config is fixed.
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
    // Failure after reservation, payload reduction and receipt insertion must
    // still roll back everything and release the reservation after bytes drop.
    for fail in [true, false] {
        let content = candidate(secret, &input, 2);
        let mut allocation = db.reserve_payload_allocation(u64::from(content.content_len()))?;
        let mut buffer = None;
        let result = db
            .commit_social_post_request_inner(
                SocialPostRequestId([1; 16]),
                &input,
                &content,
                &mut allocation,
                &mut buffer,
                fail.then(|| {
                    Box::new(|_: &crate::WriteTransactionCtx| Err(DbError::SocialPostCandidate))
                        as crate::social_post_receipt::ReceiptTestHook
                }),
            )
            .await;
        assert_eq!(result.is_err(), fail);
        assert!(buffer.is_some());
        drop(content);
        drop(buffer);
        drop(allocation);
        assert_eq!(db.payload_admission_usage().acquisitions, 0);
        assert_eq!(db.payload_admission_usage().buffers, 0);
        assert_eq!(
            db.social_post_receipt(SocialPostRequestId([1; 16]), &input)
                .await?
                .is_none(),
            fail
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn social_post_request_abort_after_commit_preserves_receipt() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cancel.redb");
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    let input = request("cancel before caller sees acknowledgment");
    let content = candidate(secret, &input, 1);
    let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task_db = db.clone();
    let task_input = input.clone();
    let task_content = content.clone();
    let task = tokio::spawn(async move {
        task_db
            .commit_social_post_request_inner(
                SocialPostRequestId([1; 16]),
                &task_input,
                &task_content,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(move || {
                        committed_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    });
                    Ok(())
                })),
            )
            .await
    });
    committed_rx.await?;
    task.abort();
    release_tx.send(())?;
    // block_in_place cannot be interrupted; whether Tokio observes abort before
    // the ready result is immaterial. Wait for actual completion before reopen.
    let _ = task.await;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate(secret, &input, 2))
            .await?
            .event_id,
        content.event_id()
    );
    Ok(())
}
