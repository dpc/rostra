use std::collections::BTreeMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use rostra_core::Timestamp;
use rostra_core::event::content_kind::{EventContentKind as _, SocialPost};
use rostra_core::event::{Event, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::retention::RetentionPolicy;

use crate::payload_runtime::{PayloadRuntime, RuntimeCursor, RuntimeLimits, RuntimeTurn};
use crate::{
    Database, PayloadAcquisitionPreparation, PayloadAdmissionConfig, PayloadAdmissionLimits,
    PayloadIngestOutcome, PayloadReservationOutcome,
};

#[tokio::test(flavor = "multi_thread")]
async fn runtime_pressure_revision_mutation_matrix_and_saturation() -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;

    let holder = RostraIdSecretKey::generate().id();
    let mut db = Database::new_in_memory(holder).await?;
    let author = RostraIdSecretKey::generate();
    let existing = post(author, 1, "existing");
    db.try_process_event_with_content(&existing).await?;
    install(&mut db, 10000, 10000, 1, 10000).await?;
    settle(&db).await?;
    let revision = || {
        db.payload_admission
            .pressure_revision
            .load(Ordering::Relaxed)
    };
    let before_duplicate = revision();
    db.try_process_event_with_content(&existing).await?;
    assert_eq!(revision(), before_duplicate);

    let incoming = post(author, 2, "incoming");
    db.try_process_event(&incoming.event).await?;
    let before_add = revision();
    let PayloadReservationOutcome::Reserved(first) =
        db.prepare_payload_acquisition(&incoming.event).await?
    else {
        panic!("fixture reservation");
    };
    assert!(revision() > before_add);
    let before_buffer = revision();
    drop(first.try_acquire_buffer()?);
    assert_eq!(revision(), before_buffer);
    drop(first);
    assert!(revision() > before_buffer);

    let PayloadReservationOutcome::Reserved(second) =
        db.prepare_payload_acquisition(&incoming.event).await?
    else {
        panic!("fixture reservation");
    };
    let buffer = second.try_acquire_buffer()?;
    let before_completion = revision();
    assert_eq!(
        db.try_process_admitted_event_content(&incoming, Some(&buffer))
            .await?,
        PayloadIngestOutcome::Processed
    );
    assert!(revision() > before_completion);
    let after_completion = revision();
    drop(buffer);
    drop(second);
    assert_eq!(revision(), after_completion);
    signed_delete(&db, author, &incoming).await?;
    assert!(revision() > after_completion);

    let before_abort = revision();
    let usage = db.get_payload_usage().await?;
    let aborted: crate::DbResult<()> = db
        .write_with(|tx| {
            let event = post(author, 3, "aborted");
            db.process_event_tx(&event.event, Timestamp::now(), tx)?;
            Err(crate::DbError::PayloadAccountingInvariant)
        })
        .await;
    assert!(aborted.is_err());
    assert!(revision() > before_abort);
    assert_eq!(db.get_payload_usage().await?, usage);

    db.payload_admission
        .pressure_revision
        .store(u64::MAX - 1, Ordering::Relaxed);
    db.payload_admission.invalidate_pressure();
    db.payload_admission.invalidate_pressure();
    assert_eq!(revision(), u64::MAX);
    let mut cursor = RuntimeCursor::default();
    let mut refused = false;
    for _ in 0..100 {
        if matches!(
            db.payload_runtime
                .as_ref()
                .unwrap()
                .turn(&db, &mut cursor)
                .await,
            Err(crate::DbError::Overflow)
        ) {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(db.get_payload_usage().await?, usage);
    drop(db);

    let account = crate::PayloadAccount::disabled(holder);
    let ledger = {
        let mut db = Database::new_in_memory(holder).await?;
        db.attach_payload_account(&account)?;
        db.payload_admission.clone()
    };
    let before_attach = ledger.pressure_revision.load(Ordering::Relaxed);
    let mut reopened = Database::new_in_memory(holder).await?;
    reopened.attach_payload_account(&account)?;
    assert!(ledger.pressure_revision.load(Ordering::Relaxed) > before_attach);
    Ok(())
}

fn post(author: RostraIdSecretKey, timestamp: u64, text: &str) -> VerifiedEventContent {
    let content = SocialPost::new_text(text.to_owned(), None, Default::default())
        .serialize_cbor()
        .unwrap();
    let signed = Event::builder_raw_content()
        .author(author.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(Timestamp::from(timestamp).to_offset_date_time().unwrap())
        .content(&content)
        .build()
        .signed_by(author);
    VerifiedEventContent::assume_verified(
        VerifiedEvent::verify_signed(author.id(), signed).unwrap(),
        content,
    )
}

async fn install(
    db: &mut Database,
    global: u64,
    author: u64,
    operations: usize,
    bytes: u64,
) -> anyhow::Result<()> {
    install_with_gc(db, global, author, operations, bytes, bytes).await
}

async fn install_with_gc(
    db: &mut Database,
    global: u64,
    author: u64,
    operations: usize,
    bytes: u64,
    gc_bytes: u64,
) -> anyhow::Result<()> {
    let config = PayloadAdmissionConfig::new(PayloadAdmissionLimits {
        database_bytes: NonZeroU64::new(global).unwrap(),
        author_bytes: NonZeroU64::new(author).unwrap(),
        overrides: BTreeMap::new(),
        in_flight_count: NonZeroUsize::new(8).unwrap(),
        in_flight_bytes: NonZeroU64::new(100_000).unwrap(),
    })
    .unwrap();
    let policy = RetentionPolicy::new(1, 1, 0, 0, 1, 0).unwrap();
    let progress = db.configure_retention_index(policy).await?;
    db.payload_runtime = Some(
        PayloadRuntime::new(
            progress.generation,
            config.identity(),
            RuntimeLimits {
                operations: NonZeroUsize::new(operations).unwrap(),
                bytes,
                gc_bytes,
                time: Duration::from_secs(1),
            },
        )
        .unwrap(),
    );
    db.payload_admission.state.lock().unwrap().config = Some(config);
    Ok(())
}

async fn settle(db: &Database) -> anyhow::Result<RuntimeCursor> {
    let runtime = db.payload_runtime.as_ref().unwrap();
    let mut cursor = RuntimeCursor::default();
    for _ in 0..1000 {
        if runtime.turn(db, &mut cursor).await? == RuntimeTurn::Wait {
            return Ok(cursor);
        }
    }
    panic!("bounded fixture maintenance failed to settle");
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_multi_cycle_turns_do_not_recycle_old_progress() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(64).unwrap())
        .await?
        .ready
    {}
    let author = RostraIdSecretKey::generate();
    for n in 1..=10 {
        db.try_process_event_with_content(&post(author, n, &format!("post {n}")))
            .await?;
    }
    let before = db.get_payload_usage().await?.unwrap();
    let high = before.logical_current_bytes / 2;
    install(
        &mut db,
        high,
        before.logical_current_bytes,
        64,
        before.logical_current_bytes,
    )
    .await?;
    let runtime = db.payload_runtime.as_ref().unwrap();
    let mut cursor = RuntimeCursor::default();
    for _ in 0..10 {
        let turn = runtime.turn(&db, &mut cursor).await?;
        let usage = db.get_payload_usage().await?.unwrap();
        if turn == RuntimeTurn::Wait
            && usage.logical_current_bytes <= high * 9 / 10
            && usage.unique_stored_bytes == usage.logical_current_bytes
        {
            return Ok(());
        }
    }
    panic!(
        "stable input did not progress: {cursor:?}, usage={:?}",
        db.get_payload_usage().await?
    );
}

async fn prepare_with_worker(
    db: &Database,
    event: &VerifiedEvent,
) -> anyhow::Result<PayloadReservationOutcome> {
    let runtime = db.payload_runtime.as_ref().unwrap();
    Ok(tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = db.prepare_payload_acquisition(event) => result,
            result = runtime.run(db) => panic!("worker stopped: {result:?}"),
        }
    })
    .await??)
}

async fn prepare_detailed_with_worker(
    db: &Database,
    event: &VerifiedEvent,
) -> anyhow::Result<PayloadAcquisitionPreparation> {
    let runtime = db.payload_runtime.as_ref().unwrap();
    Ok(tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = db.prepare_payload_acquisition_detailed(event) => result,
            result = runtime.run(db) => panic!("worker stopped: {result:?}"),
        }
    })
    .await??)
}

async fn is_missing(db: &Database, event: &VerifiedEventContent) -> anyhow::Result<bool> {
    Ok(db
        .read_with(|tx| {
            Ok(matches!(
                tx.open_table(&crate::events_content_state::TABLE)?
                    .get(&event.event_id().to_short())?
                    .map(|row| row.value()),
                Some(crate::EventContentState::Missing { .. })
            ))
        })
        .await?)
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_ranked_rejection_retains_better_incoming_in_author_and_global_scopes()
-> anyhow::Result<()> {
    for author_pressure in [false, true] {
        let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
        let author = RostraIdSecretKey::generate();
        let old = post(author, 1, "same");
        let better = post(author, 3, "same");
        let worse = post(author, 2, "same");
        let bytes = u64::from(old.content_len());
        db.try_process_event_with_content(&old).await?;
        install(
            &mut db,
            if author_pressure { 10000 } else { bytes + 1 },
            if author_pressure { bytes + 1 } else { 10000 },
            1,
            bytes,
        )
        .await?;
        settle(&db).await?;
        // Shared-store reuse must first make logical room, just like a fetch.
        assert!(matches!(
            prepare_with_worker(&db, &better.event).await?,
            PayloadReservationOutcome::Unneeded
        ));
        assert!(
            db.get_event_content(better.event_id().to_short())
                .await
                .is_some()
        );
        assert!(matches!(
            prepare_with_worker(&db, &worse.event).await?,
            PayloadReservationOutcome::Unneeded
        ));
        db.read_with(|tx| {
            assert_eq!(
                tx.open_table(&crate::events_quota_pruned::TABLE)?
                    .get(&worse.event_id().to_short())?
                    .unwrap()
                    .value()
                    .reason,
                if author_pressure {
                    crate::QuotaPruneReason::AuthorQuota
                } else {
                    crate::QuotaPruneReason::GlobalQuota
                }
            );
            Ok(())
        })
        .await?;
        assert!(
            db.get_event_content(better.event_id().to_short())
                .await
                .is_some()
        );
        assert_eq!(
            db.get_payload_usage().await?.unwrap().logical_current_bytes,
            bytes
        );
        let usage = db.payload_admission_usage();
        assert_eq!(
            (usage.acquisitions, usage.buffers, usage.buffer_bytes),
            (0, 0, 0)
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_ranked_rejection_survives_duplicate_shared_hash_ingress_and_replay()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let retained = post(author, 20, "same");
    let rejected = post(author, 10, "same");
    let bytes = u64::from(retained.content_len());
    db.try_process_event_with_content(&retained).await?;
    install(&mut db, bytes + 1, 10000, 1, bytes).await?;
    settle(&db).await?;
    let mut signals = db.quota_pruned_tx.subscribe();
    assert!(matches!(
        prepare_with_worker(&db, &rejected.event).await?,
        PayloadReservationOutcome::Unneeded
    ));
    assert_eq!(signals.try_recv()?, rejected.event_id().to_short());
    for _ in 0..32 {
        assert!(matches!(
            db.prepare_payload_acquisition(&rejected.event).await?,
            PayloadReservationOutcome::Unneeded
        ));
        assert_eq!(
            db.try_process_admitted_event_content(&rejected, None)
                .await?,
            PayloadIngestOutcome::Unchanged
        );
        assert_eq!(
            db.try_materialize_stored_payload(rejected.event_id().to_short())
                .await?,
            PayloadIngestOutcome::Unchanged
        );
    }
    assert!(signals.try_recv().is_err());
    let decision = db
        .read_with(|tx| {
            Ok(tx
                .open_table(&crate::events_quota_pruned::TABLE)?
                .get(&rejected.event_id().to_short())?
                .unwrap()
                .value())
        })
        .await?;
    // Replay discards runtime indexes but preserves source decisions before
    // processing envelopes whose shared bytes still exist.
    db.payload_runtime = None;
    db.payload_admission.state.lock().unwrap().config = None;
    for _ in 0..2 {
        db.write_with(|tx| Database::prepare_total_migration(tx, 31))
            .await?;
        db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
        db.try_process_event_with_content(&rejected).await?;
        db.read_with(|tx| {
            assert!(
                tx.open_table(&crate::events::TABLE)?
                    .get(&rejected.event_id().to_short())?
                    .is_some()
            );
            assert_eq!(
                tx.open_table(&crate::events_quota_pruned::TABLE)?
                    .get(&rejected.event_id().to_short())?
                    .unwrap()
                    .value(),
                decision
            );
            assert!(matches!(
                tx.open_table(&crate::events_content_state::TABLE)?
                    .get(&rejected.event_id().to_short())?
                    .unwrap()
                    .value(),
                crate::EventContentState::Pruned
            ));
            assert!(
                tx.open_table(&crate::events_content_missing::TABLE)?
                    .first()?
                    .is_none()
            );
            assert!(
                tx.open_table(&crate::social_posts::TABLE)?
                    .get(&rejected.event_id().to_short())?
                    .is_none()
            );
            assert_eq!(
                tx.open_table(&crate::content_rc::TABLE)?
                    .get(&rejected.content_hash())?
                    .unwrap()
                    .value(),
                1
            );
            let usage = tx
                .open_table(&crate::ids_data_usage::TABLE)?
                .get(&author.id())?
                .unwrap()
                .value();
            assert_eq!(usage.current_content_size, bytes);
            assert_eq!(usage.pruned_payload_size, bytes);
            assert!(
                tx.open_table(&crate::events_retention_origins::TABLE)?
                    .get(&rejected.event_id().to_short())?
                    .unwrap()
                    .value()
                    .materialized_at
                    .is_none()
            );
            Ok(())
        })
        .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_ranked_rejection_needs_boundary_not_just_protected_usage() -> anyhow::Result<()> {
    for has_boundary in [false, true] {
        let local = RostraIdSecretKey::generate();
        let mut db = Database::new_in_memory(local.id()).await?;
        let author = RostraIdSecretKey::generate();
        let protected = post(local, 1, "same");
        let boundary = post(author, 20, "same");
        let incoming = post(author, 10, "same");
        db.try_process_event_with_content(&protected).await?;
        if has_boundary {
            db.try_process_event_with_content(&boundary).await?;
        }
        let bytes = u64::from(protected.content_len());
        let cap = bytes * if has_boundary { 2 } else { 1 } + 1;
        install(&mut db, cap, 10000, 1, bytes).await?;
        settle(&db).await?;
        if has_boundary {
            assert!(matches!(
                prepare_with_worker(&db, &incoming.event).await?,
                PayloadReservationOutcome::Unneeded
            ));
        } else {
            let prepare = db.prepare_payload_acquisition(&incoming.event);
            tokio::pin!(prepare);
            assert!(futures::poll!(&mut prepare).is_pending());
            settle(&db).await?;
            assert!(futures::poll!(&mut prepare).is_pending());
            assert!(is_missing(&db, &incoming).await?);
            assert_eq!(db.payload_admission_usage().pending_demands, 1);
        }
        assert!(
            db.get_event_content(protected.event_id().to_short())
                .await
                .is_some()
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_ranked_rejection_ignores_reservation_only_pressure_and_live_lease()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let retained = post(author, 20, "same");
    let incoming = post(author, 10, "diff");
    let other = post(author, 30, "else");
    db.try_process_event_with_content(&retained).await?;
    let bytes = u64::from(retained.content_len());
    install(&mut db, 2 * bytes + 1, 10000, 1, bytes).await?;
    settle(&db).await?;
    let PayloadReservationOutcome::Reserved(reservation) =
        db.prepare_payload_acquisition(&other.event).await?
    else {
        panic!("room");
    };
    {
        let prepare = db.prepare_payload_acquisition(&incoming.event);
        tokio::pin!(prepare);
        assert!(futures::poll!(&mut prepare).is_pending());
        settle(&db).await?;
        assert!(is_missing(&db, &incoming).await?);
    }
    drop(reservation);
    let PayloadReservationOutcome::Reserved(reservation) =
        prepare_with_worker(&db, &incoming.event).await?
    else {
        panic!("released room");
    };
    {
        let duplicate = db.prepare_payload_acquisition(&incoming.event);
        tokio::pin!(duplicate);
        assert!(futures::poll!(&mut duplicate).is_pending());
        settle(&db).await?;
        assert_eq!(db.payload_admission_usage().pending_demands, 0);
        assert!(is_missing(&db, &incoming).await?);
    }
    drop(reservation);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_cap_minus_one_prepares_preempts_reserves_and_ingests() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let old = post(author, 1, "old");
    let incoming = post(author, 2, "new");
    db.try_process_event_with_content(&old).await?;
    let bytes = u64::from(old.content_len());
    install(&mut db, bytes + 1, 10000, 5, bytes).await?;
    assert!(db.get_payload_usage().await?.is_none());

    let PayloadReservationOutcome::Reserved(reservation) =
        prepare_with_worker(&db, &incoming.event).await?
    else {
        panic!("incoming acquisition did not reserve");
    };
    assert!(
        db.get_event_content(old.event_id().to_short())
            .await
            .is_none()
    );
    let usage = db.payload_admission_usage();
    assert_eq!(usage.logical_reserved_bytes, bytes);
    assert_eq!(usage.pending_demands, 0);
    assert_eq!(usage.buffers, 0);
    let buffer = reservation.try_acquire_buffer()?;
    assert_eq!(
        db.try_process_admitted_event_content(&incoming, Some(&buffer))
            .await?,
        PayloadIngestOutcome::Processed,
    );
    drop(buffer);
    drop(reservation);
    assert_eq!(db.payload_admission_usage(), Default::default());
    let usage = db.get_payload_usage().await?.unwrap();
    assert_eq!(usage.logical_current_bytes, bytes);
    // Acquisition cancels its helper's worker as soon as reservation succeeds.
    // Physical collection is a later phase, not an acquisition barrier.
    settle(&db).await?;
    let usage = db.get_payload_usage().await?.unwrap();
    // Quota-only collection removes the old unique value, not physical pages.
    assert_eq!(usage.unique_stored_bytes, bytes);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_independent_maintenance_finishes_with_single_operation_turns() -> anyhow::Result<()>
{
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    for time in 1..5 {
        db.try_process_event_with_content(&post(RostraIdSecretKey::generate(), time, "same"))
            .await?;
    }
    install(&mut db, 10000, 10000, 1, 10000).await?;
    let mut cursor = settle(&db).await?;
    assert!(db.get_payload_usage().await?.is_some());
    assert!(db.retention_index_progress().await?.unwrap().ready);
    for _ in 0..3 {
        wait_cursor(&db, &mut cursor).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_paused_prepare_deduplicates_and_cancels_without_buffers() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let old = post(author, 1, "old");
    let incoming = post(author, 2, "new");
    db.try_process_event_with_content(&old).await?;
    install(&mut db, u64::from(old.content_len()) + 1, 10000, 5, 10000).await?;
    let mut cursor = settle(&db).await?;
    {
        let first = db.prepare_payload_acquisition(&incoming.event);
        let second = db.prepare_payload_acquisition(&incoming.event);
        tokio::pin!(first, second);
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        let usage = db.payload_admission_usage();
        assert_eq!(usage.pending_demands, 1);
        assert_eq!(
            usage.pending_demand_bytes,
            u64::from(incoming.content_len())
        );
        assert_eq!(usage.buffers, 0);
        assert_eq!(usage.buffer_bytes, 0);
        assert_eq!(usage.logical_reserved_bytes, 0);
    }
    assert_eq!(db.payload_admission_usage().pending_demands, 0);
    wait_cursor(&db, &mut cursor).await?;
    assert!(
        db.get_event_content(old.event_id().to_short())
            .await
            .is_some()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_expired_prepare_does_not_renew_or_prune() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let old = post(author, 1, "old");
    let incoming = post(author, 2, "new");
    db.try_process_event_with_content(&old).await?;
    install(&mut db, u64::from(old.content_len()) + 1, 10000, 5, 10000).await?;
    settle(&db).await?;
    let prepare = db.prepare_payload_acquisition(&incoming.event);
    tokio::pin!(prepare);
    assert!(futures::poll!(&mut prepare).is_pending());
    db.payload_admission
        .demands
        .lock()
        .unwrap()
        .entries
        .get_mut(&incoming.event_id())
        .unwrap()
        .expires = Timestamp::now();
    let result = tokio::time::timeout(Duration::from_secs(2), prepare).await??;
    assert!(matches!(result, PayloadReservationOutcome::Deferred(_)));
    assert_eq!(db.payload_admission_usage().pending_demands, 0);
    assert!(
        db.get_event_content(old.event_id().to_short())
            .await
            .is_some()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_shared_store_reuse_waits_without_retaining_a_copy() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let old = post(author, 1, "same");
    let incoming = post(author, 2, "same");
    db.try_process_event_with_content(&old).await?;
    let bytes = u64::from(old.content_len());
    install(&mut db, bytes + 1, 10000, 5, bytes).await?;
    settle(&db).await?;
    {
        let prepare = db.prepare_payload_acquisition(&incoming.event);
        tokio::pin!(prepare);
        assert!(futures::poll!(&mut prepare).is_pending());
        assert_eq!(db.payload_admission_usage().pending_demands, 1);
        assert_eq!(db.payload_admission_usage().buffers, 0);
    }
    assert!(matches!(
        prepare_detailed_with_worker(&db, &incoming.event).await?,
        PayloadAcquisitionPreparation::Materialized,
    ));
    assert!(matches!(
        db.prepare_payload_acquisition(&incoming.event).await?,
        PayloadReservationOutcome::Unneeded,
    ));
    assert!(
        db.get_event_content(incoming.event_id().to_short())
            .await
            .is_some()
    );
    assert_eq!(
        db.get_payload_usage().await?.unwrap().unique_stored_bytes,
        bytes
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_alternate_author_acquisition_passes_an_exhausted_higher_rank() -> anyhow::Result<()>
{
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let a = RostraIdSecretKey::generate();
    let b = RostraIdSecretKey::generate();
    let high_old = post(a, 95, "old");
    let high = post(a, 90, "new");
    let low_old = post(b, 1, "old");
    let low = post(b, 50, "new");
    db.try_process_event_with_content(&high_old).await?;
    db.try_process_event_with_content(&low_old).await?;
    let bytes = u64::from(low.content_len());
    install(&mut db, 10000, bytes + 1, 1, bytes).await?;
    settle(&db).await?;
    let high_prepare = db.prepare_payload_acquisition(&high.event);
    tokio::pin!(high_prepare);
    assert!(futures::poll!(&mut high_prepare).is_pending());
    assert!(matches!(
        prepare_with_worker(&db, &low.event).await?,
        PayloadReservationOutcome::Reserved(_),
    ));
    assert!(
        db.get_event_content(high_old.event_id().to_short())
            .await
            .is_some()
    );
    assert!(
        db.get_event_content(low_old.event_id().to_short())
            .await
            .is_none()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_byte_blockage_sleeps_and_runner_cancellation_releases_exclusivity()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let old = post(author, 1, "old");
    let incoming = post(author, 2, "new");
    db.try_process_event_with_content(&old).await?;
    install(&mut db, u64::from(old.content_len()) + 1, 10000, 5, 1).await?;
    let mut cursor = settle(&db).await?;
    let prepare = db.prepare_payload_acquisition(&incoming.event);
    tokio::pin!(prepare);
    assert!(futures::poll!(&mut prepare).is_pending());
    let runtime = db.payload_runtime.as_ref().unwrap();
    for _ in 0..10 {
        wait_cursor(&db, &mut cursor).await?;
    }
    for _ in 0..2 {
        let run = runtime.run(&db);
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        assert!(runtime.run(&db).await.is_err());
        // Even repeated coalesced wakes cannot bypass minimum spacing.
        for _ in 0..10 {
            db.payload_admission.changed.notify_waiters();
            assert!(futures::poll!(&mut run).is_pending());
        }
    }
    assert!(
        db.get_event_content(old.event_id().to_short())
            .await
            .is_some()
    );
    Ok(())
}

async fn wait_cursor(db: &Database, cursor: &mut RuntimeCursor) -> anyhow::Result<()> {
    for _ in 0..1000 {
        if db
            .payload_runtime
            .as_ref()
            .unwrap()
            .turn(db, cursor)
            .await?
            == RuntimeTurn::Wait
        {
            return Ok(());
        }
    }
    panic!("stable bounded recovery cycle did not sleep");
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_acquisition_crosses_a_previously_promoted_future_prefix() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let future = Timestamp::now().saturating_add_secs(3600);
    let mut protected = Vec::new();
    for time in 1..=12 {
        let event = post(author, time, "same");
        db.write_with(|tx| {
            db.process_event_tx(&event.event, Timestamp::now(), tx)?;
            db.process_event_content_tx(&event, future, tx)?;
            Ok(())
        })
        .await?;
        protected.push(event);
    }
    let old = post(author, 20, "same");
    let incoming = post(author, 50, "next");
    db.try_process_event_with_content(&old).await?;
    let bytes = u64::from(old.content_len());
    install(&mut db, 13 * bytes + 1, 10000, 1, bytes).await?;
    let one = NonZeroUsize::new(1).unwrap();
    while !db.rebuild_retention_index(one).await?.unwrap().ready {}
    while db
        .promote_retention_grace(crate::RetentionClock::Trusted(future), one)
        .await?
        .unwrap()
        .visited
        != 0
    {}
    // Model a backwards clock after promotion without demoting the index.
    // Fresh candidate checks must skip all twelve future materializations,
    // retaining bounded continuation instead of retrying the first row.
    assert!(matches!(
        prepare_with_worker(&db, &incoming.event).await?,
        PayloadReservationOutcome::Reserved(_),
    ));
    assert!(
        db.get_event_content(old.event_id().to_short())
            .await
            .is_none()
    );
    for event in protected {
        assert!(
            db.get_event_content(event.event_id().to_short())
                .await
                .is_some()
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_joined_abort_releases_the_last_database_owner() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    install(&mut db, 10000, 10000, 5, 10000).await?;
    settle(&db).await?;
    let db = std::sync::Arc::new(db);
    let weak = std::sync::Arc::downgrade(&db);
    let (started, running) = tokio::sync::oneshot::channel();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(async move {
        let run = db.payload_runtime.as_ref().unwrap().run(&db);
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        started.send(()).unwrap();
        run.await
    });
    running.await?;
    assert!(weak.upgrade().is_some());
    tasks.abort_all();
    assert!(tasks.join_next().await.unwrap().unwrap_err().is_cancelled());
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_startup_author_first_hysteresis_and_shared_store_pins() -> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let mut authored = Vec::new();
    let mut global = Vec::new();
    for time in 100..111 {
        let event = post(author, time, "same");
        db.try_process_event_with_content(&event).await?;
        authored.push(event);
    }
    for time in 1..11 {
        let event = post(other, time, "same");
        db.try_process_event_with_content(&event).await?;
        global.push(event);
    }
    let bytes = u64::from(authored[0].content_len());
    // Two author reductions cross high, then low. Only then may global pressure
    // remove three older events from the other author.
    install(
        &mut db,
        bytes * 18 + bytes / 2,
        bytes * 10 + bytes / 2,
        1,
        bytes,
    )
    .await?;
    let mut cursor = RuntimeCursor::default();
    let mut previous = bytes * 21;
    let mut reductions = 0;
    for _ in 0..3000 {
        db.payload_runtime
            .as_ref()
            .unwrap()
            .turn(&db, &mut cursor)
            .await?;
        let Some(usage) = db.get_payload_usage().await? else {
            continue;
        };
        assert!(previous - usage.logical_current_bytes <= bytes);
        if usage.logical_current_bytes < previous {
            reductions += 1;
            if reductions <= 2 {
                for event in &global {
                    assert!(
                        db.get_event_content(event.event_id().to_short())
                            .await
                            .is_some()
                    );
                }
            }
        }
        previous = usage.logical_current_bytes;
        if reductions == 5 {
            break;
        }
    }
    assert_eq!(reductions, 5);
    for _ in 0..3 {
        wait_cursor(&db, &mut cursor).await?;
    }
    let usage = db.get_payload_usage().await?.unwrap();
    assert_eq!(usage.logical_current_bytes, bytes * 16);
    assert_eq!(usage.unique_stored_bytes, bytes);
    db.write_with(|tx| {
        for event in &authored[..2] {
            assert_eq!(
                tx.open_table(&crate::events_quota_pruned::TABLE)?
                    .get(&event.event_id().to_short())?
                    .unwrap()
                    .value_try()?
                    .reason,
                crate::QuotaPruneReason::AuthorQuota
            );
        }
        for event in &global[..3] {
            assert_eq!(
                tx.open_table(&crate::events_quota_pruned::TABLE)?
                    .get(&event.event_id().to_short())?
                    .unwrap()
                    .value_try()?
                    .reason,
                crate::QuotaPruneReason::GlobalQuota
            );
        }
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_global_hysteresis_survives_turns_and_runner_cursor_replacement()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    for time in 1..12 {
        db.try_process_event_with_content(&post(author, time, "same"))
            .await?;
    }
    let bytes = u64::from(post(author, 1, "same").content_len());
    install(&mut db, bytes * 10 + bytes / 2, 10000, 1, bytes).await?;
    let mut cursor = RuntimeCursor::default();
    for _ in 0..1000 {
        db.payload_runtime
            .as_ref()
            .unwrap()
            .turn(&db, &mut cursor)
            .await?;
        if db
            .get_payload_usage()
            .await?
            .is_some_and(|u| u.logical_current_bytes == bytes * 10)
        {
            break;
        }
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().logical_current_bytes,
        bytes * 10
    );
    // A new caller-owned runner has no scheduler cursor, but uses the same
    // runtime incarnation and must still reach floor(90%) below the high water.
    cursor = RuntimeCursor::default();
    for _ in 0..10 {
        wait_cursor(&db, &mut cursor).await?;
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().logical_current_bytes,
        bytes * 9
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_protected_unknown_and_oversized_minimum_sleep_without_eviction()
-> anyhow::Result<()> {
    let holder = RostraIdSecretKey::generate();
    let mut db = Database::new_in_memory(holder.id()).await?;
    let author = RostraIdSecretKey::generate();
    let local = post(holder, 1, "local");
    let unknown = post(author, 2, "unknown");
    let minimum = post(author, 3, &"large".repeat(100));
    let later = post(author, 4, "small");
    let alternate = post(RostraIdSecretKey::generate(), 5, "small");
    for event in [&local, &unknown, &minimum, &later, &alternate] {
        db.try_process_event_with_content(event).await?;
    }
    db.write_with(|tx| {
        tx.open_table(&crate::events_retention_origins::TABLE)?
            .remove(&unknown.event_id().to_short())?;
        Ok(())
    })
    .await?;
    install(&mut db, 1, 1, 1, u64::from(later.content_len())).await?;
    let mut cursor = settle(&db).await?;
    for _ in 0..4 {
        wait_cursor(&db, &mut cursor).await?;
    }
    for event in [&local, &unknown, &minimum, &later] {
        assert!(
            db.get_event_content(event.event_id().to_short())
                .await
                .is_some()
        );
    }
    assert!(
        db.get_event_content(alternate.event_id().to_short())
            .await
            .is_none()
    );
    // An eligible minimum is never bypassed merely to fit the byte budget.
    assert_eq!(db.payload_admission_usage().pending_demands, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_reservations_trigger_pressure_and_cancellation_stops_low_water()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    for time in 1..11 {
        db.try_process_event_with_content(&post(author, time, "same"))
            .await?;
    }
    let bytes = u64::from(post(author, 1, "same").content_len());
    install(&mut db, 10000, 10000, 1, bytes).await?;
    settle(&db).await?;
    let incoming = post(author, 50, "diff");
    let PayloadReservationOutcome::Reserved(reservation) =
        db.prepare_payload_acquisition(&incoming.event).await?
    else {
        panic!("fixture reservation");
    };
    // Test-only replacement creates startup over-cap usage including an existing
    // reservation. Production exposes no configuration replacement API.
    install(&mut db, bytes * 10 + bytes / 2, 10000, 1, bytes).await?;
    let mut cursor = RuntimeCursor::default();
    for _ in 0..1000 {
        db.payload_runtime
            .as_ref()
            .unwrap()
            .turn(&db, &mut cursor)
            .await?;
        if db.get_payload_usage().await?.unwrap().logical_current_bytes == bytes * 9 {
            break;
        }
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().logical_current_bytes,
        bytes * 9
    );
    assert_eq!(db.payload_admission_usage().logical_reserved_bytes, bytes);
    let revision = db
        .payload_admission
        .pressure_revision
        .load(std::sync::atomic::Ordering::Relaxed);
    drop(reservation);
    assert!(
        db.payload_admission
            .pressure_revision
            .load(std::sync::atomic::Ordering::Relaxed)
            > revision
    );
    for _ in 0..5 {
        wait_cursor(&db, &mut cursor).await?;
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().logical_current_bytes,
        bytes * 9
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_quota_gc_skips_oversized_hashes_and_retries_bounded_sweeps() -> anyhow::Result<()>
{
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let mut values = Vec::new();
    for time in 1..21 {
        let text = if time % 2 == 0 {
            format!("small {time}")
        } else {
            format!("large {time} {}", "x".repeat(500))
        };
        let event = post(author, time, &text);
        db.try_process_event_with_content(&event).await?;
        values.push(event);
    }
    values.sort_by_key(|event| event.content_hash());
    let small_limit = 100;
    assert!(
        values
            .windows(2)
            .any(|pair| u64::from(pair[0].content_len()) > small_limit
                && u64::from(pair[1].content_len()) <= small_limit)
    );
    install_with_gc(&mut db, 1, 100000, 1, 100000, small_limit).await?;
    let mut cursor = RuntimeCursor::default();
    for _ in 0..50 {
        wait_cursor(&db, &mut cursor).await?;
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().logical_current_bytes,
        0
    );
    let expected: u64 = values
        .iter()
        .filter(|e| u64::from(e.content_len()) > small_limit)
        .map(|e| u64::from(e.content_len()))
        .sum();
    assert_eq!(
        db.get_payload_usage().await?.unwrap().unique_stored_bytes,
        expected
    );
    db.write_with(|tx| {
        for event in &values {
            let oversized = u64::from(event.content_len()) > small_limit;
            assert_eq!(
                tx.open_table(&crate::content_store::TABLE)?
                    .get(&event.content_hash())?
                    .is_some(),
                oversized
            );
            assert_eq!(
                tx.open_table(&crate::content_quota_gc::TABLE)?
                    .get(&event.content_hash())?
                    .is_some(),
                oversized
            );
            assert!(
                tx.open_table(&crate::content_quota_hashes::TABLE)?
                    .get(&event.content_hash())?
                    .is_some()
            );
        }
        Ok(())
    })
    .await?;
    for _ in 0..3 {
        wait_cursor(&db, &mut cursor).await?;
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().unique_stored_bytes,
        expected
    );
    install_with_gc(&mut db, 1, 100000, 1, 100000, 100000).await?;
    cursor = RuntimeCursor::default();
    for _ in 0..3 {
        wait_cursor(&db, &mut cursor).await?;
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().unique_stored_bytes,
        0
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_nonfitting_no_victim_demand_churn_does_not_starve_startup_pressure()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    for time in 10..21 {
        db.try_process_event_with_content(&post(author, time, "same"))
            .await?;
    }
    let incoming = post(author, 1, "diff");
    db.try_process_event(&incoming.event).await?;
    let bytes = u64::from(incoming.content_len());
    install(&mut db, 10000, 10000, 1, bytes).await?;
    settle(&db).await?;
    install(&mut db, bytes * 10 + bytes / 2, 10000, 1, bytes).await?;
    let generation = db.retention_index_progress().await?.unwrap().generation;
    let mut cursor = RuntimeCursor::default();
    let mut pruned = false;
    for _ in 0..1000 {
        // Replacing low-ranked nonrenewed intent cannot bar general relief.
        let owner = db
            .register_payload_demand(incoming.event_id(), generation)
            .await?;
        db.payload_runtime
            .as_ref()
            .unwrap()
            .turn(&db, &mut cursor)
            .await?;
        drop(owner);
        if db.get_payload_usage().await?.unwrap().logical_current_bytes < bytes * 11 {
            pruned = true;
            break;
        }
    }
    assert!(pruned);
    assert!(
        db.get_event_content(incoming.event_id().to_short())
            .await
            .is_none()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_pressure_checked_rollback_and_stale_incarnation_fail_closed() -> anyhow::Result<()>
{
    use std::time::Instant;

    use crate::payload_pressure::{PressureCursor, PressureRequest, PressureStep, PressureWorker};

    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let event = post(RostraIdSecretKey::generate(), 1, "old");
    db.try_process_event_with_content(&event).await?;
    install(&mut db, 10000, 10000, 1, 10000).await?;
    settle(&db).await?;
    install(&mut db, 1, 10000, 1, 10000).await?;
    let generation = db.retention_index_progress().await?.unwrap().generation;
    let config = db
        .payload_admission
        .state
        .lock()
        .unwrap()
        .config
        .as_ref()
        .unwrap()
        .identity();
    let request = || PressureRequest {
        generation,
        config: &config,
        max_bytes: 10000,
        deadline: Instant::now() + Duration::from_secs(1),
    };
    let worker = PressureWorker::default();
    let mut cursor = PressureCursor::default();
    let before = db.get_payload_usage().await?;
    let mut aborted = false;
    for _ in 0..100 {
        let result = worker
            .step_with(&db, request(), &mut cursor, Timestamp::now, || {
                Err(crate::DbError::PayloadAccountingInvariant)
            })
            .await;
        if result.is_err() {
            aborted = true;
            break;
        }
    }
    assert!(aborted);
    assert_eq!(db.get_payload_usage().await?, before);
    assert!(
        db.get_event_content(event.event_id().to_short())
            .await
            .is_some()
    );
    db.write_with(|tx| {
        assert!(
            tx.open_table(&crate::events_quota_pruned::TABLE)?
                .get(&event.event_id().to_short())?
                .is_none()
        );
        Ok(())
    })
    .await?;
    let replacement = PressureWorker::default();
    assert_eq!(
        replacement.step(&db, request(), &mut cursor,).await?,
        PressureStep::Continue
    );
    assert!(worker.step(&db, request(), &mut cursor,).await.is_err());
    let mut committed = false;
    for _ in 0..100 {
        if matches!(
            replacement.step(&db, request(), &mut cursor,).await?,
            PressureStep::Pruned { .. }
        ) {
            committed = true;
            break;
        }
    }
    assert!(committed);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_reopen_starts_fresh_pressure_instead_of_inheriting_old_low_water()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("pressure.db");
    let holder = RostraIdSecretKey::generate().id();
    let author = RostraIdSecretKey::generate();
    let bytes = u64::from(post(author, 1, "same").content_len());
    {
        let mut db = Database::open(&path, holder).await?;
        for time in 1..12 {
            db.try_process_event_with_content(&post(author, time, "same"))
                .await?;
        }
        install(&mut db, bytes * 10 + bytes / 2, 10000, 1, bytes).await?;
        let mut cursor = RuntimeCursor::default();
        for _ in 0..1000 {
            db.payload_runtime
                .as_ref()
                .unwrap()
                .turn(&db, &mut cursor)
                .await?;
            if db
                .get_payload_usage()
                .await?
                .is_some_and(|u| u.logical_current_bytes == bytes * 10)
            {
                break;
            }
        }
        assert_eq!(
            db.get_payload_usage().await?.unwrap().logical_current_bytes,
            bytes * 10
        );
    }
    {
        let mut db = Database::open(&path, holder).await?;
        assert!(db.payload_runtime.is_none());
        assert!(db.payload_admission.state.lock().unwrap().config.is_none());
        install(&mut db, bytes * 10 + bytes / 2, 10000, 1, bytes).await?;
        let mut cursor = settle(&db).await?;
        for _ in 0..3 {
            wait_cursor(&db, &mut cursor).await?;
        }
        assert_eq!(
            db.get_payload_usage().await?.unwrap().logical_current_bytes,
            bytes * 10
        );
    }
    Ok(())
}

async fn signed_delete(
    db: &Database,
    author: RostraIdSecretKey,
    event: &VerifiedEventContent,
) -> anyhow::Result<()> {
    let signed = Event::builder_raw_content()
        .author(author.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(Timestamp::from(1000).to_offset_date_time().unwrap())
        .delete(event.event_id().to_short())
        .build()
        .signed_by(author);
    db.try_process_event(&VerifiedEvent::verify_signed(author.id(), signed).unwrap())
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_general_pressure_crosses_future_frontiers_with_one_operation_turns()
-> anyhow::Result<()> {
    let mut db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let author = RostraIdSecretKey::generate();
    let future = Timestamp::now().saturating_add_secs(3600);
    let mut protected = Vec::new();
    for time in 1..=12 {
        let event = post(author, time, "same");
        db.write_with(|tx| {
            db.process_event_tx(&event.event, Timestamp::now(), tx)?;
            db.process_event_content_tx(&event, future, tx)?;
            Ok(())
        })
        .await?;
        protected.push(event);
    }
    let first = post(author, 20, "same");
    let second = post(author, 30, "same");
    db.try_process_event_with_content(&first).await?;
    db.try_process_event_with_content(&second).await?;
    let bytes = u64::from(first.content_len());
    install(&mut db, 13 * bytes + bytes / 2, 10000, 1, bytes).await?;
    let one = NonZeroUsize::new(1).unwrap();
    while !db.rebuild_retention_index(one).await?.unwrap().ready {}
    while db
        .promote_retention_grace(crate::RetentionClock::Trusted(future), one)
        .await?
        .unwrap()
        .visited
        != 0
    {}
    let mut cursor = RuntimeCursor::default();
    for _ in 0..5 {
        wait_cursor(&db, &mut cursor).await?;
    }
    for event in &protected {
        assert!(
            db.get_event_content(event.event_id().to_short())
                .await
                .is_some()
        );
    }
    for event in [&first, &second] {
        assert!(
            db.get_event_content(event.event_id().to_short())
                .await
                .is_none()
        );
    }
    assert_eq!(
        db.get_payload_usage().await?.unwrap().logical_current_bytes,
        bytes * 12
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_quota_gc_requeues_final_missing_release_but_preserves_legacy_and_local_history()
-> anyhow::Result<()> {
    let holder = RostraIdSecretKey::generate();
    let mut db = Database::new_in_memory(holder.id()).await?;
    let author = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let retained = post(author, 1, "shared");
    let missing = post(other, 2, "shared");
    let protected = post(author, 3, "protected");
    let local = post(holder, 4, "protected");
    let legacy = post(author, 5, "unrelated deleted");
    for event in [&retained, &protected, &legacy] {
        db.try_process_event_with_content(event).await?;
    }
    for event in [&missing, &local] {
        db.try_process_event(&event.event).await?;
    }
    signed_delete(&db, holder, &local).await?;
    signed_delete(&db, author, &legacy).await?;
    install(&mut db, 1, 10000, 1, 10000).await?;
    let mut cursor = RuntimeCursor::default();
    for _ in 0..10 {
        wait_cursor(&db, &mut cursor).await?;
    }
    db.write_with(|tx| {
        assert!(
            tx.open_table(&crate::content_store::TABLE)?
                .get(&retained.content_hash())?
                .is_some()
        );
        assert!(
            tx.open_table(&crate::content_quota_gc::TABLE)?
                .get(&retained.content_hash())?
                .is_none()
        );
        Ok(())
    })
    .await?;
    signed_delete(&db, other, &missing).await?;
    for _ in 0..3 {
        wait_cursor(&db, &mut cursor).await?;
    }
    db.write_with(|tx| {
        assert!(
            tx.open_table(&crate::content_store::TABLE)?
                .get(&retained.content_hash())?
                .is_none()
        );
        assert!(
            tx.open_table(&crate::content_store::TABLE)?
                .get(&protected.content_hash())?
                .is_some()
        );
        assert!(
            tx.open_table(&crate::content_store::TABLE)?
                .get(&legacy.content_hash())?
                .is_some()
        );
        assert!(
            tx.open_table(&crate::content_quota_hashes::TABLE)?
                .get(&legacy.content_hash())?
                .is_none()
        );
        Ok(())
    })
    .await?;
    Ok(())
}
