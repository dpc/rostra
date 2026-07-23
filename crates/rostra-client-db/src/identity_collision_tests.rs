use std::panic::AssertUnwindSafe;

use futures::FutureExt as _;
use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{Event, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RestRostraId, RostraId, RostraIdSecretKey, ShortRostraId, ToShort as _};
use rostra_core::{ContentHash, ShortEventId};
use rostra_util_error::BoxedErrorResult;
use tempfile::tempdir;

use crate::{
    Database, DbError, DbResult, InsertEventOutcome, content_rc, content_store, db_version, events,
    events_by_time, events_content_missing, events_content_state, events_heads, events_missing,
    events_received_at, ids_data_usage, ids_full, social_posts, social_posts_by_time,
};

#[derive(Debug, PartialEq, Eq)]
struct IngestionSnapshot {
    identity_rest: Option<RestRostraId>,
    event_count: usize,
    events_by_time_count: usize,
    missing_parent_count: usize,
    head_count: usize,
    content_count: usize,
    content_state_count: usize,
    content_queue_count: usize,
    content_rc_count: usize,
    usage_count: usize,
    reception_count: usize,
    post_count: usize,
    post_time_count: usize,
    accepted_event_present: bool,
    rejected_event_present: bool,
    accepted_content_present: bool,
    rejected_content_present: bool,
}

fn colliding_ids() -> (ShortRostraId, RostraId, RostraId) {
    let first = RostraIdSecretKey::from_bytes([41; 32]).id();
    let second = RostraIdSecretKey::from_bytes([42; 32]).id();
    let (prefix, first_rest) = first.split();
    let (_, second_rest) = second.split();
    assert_ne!(first_rest, second_rest);
    (
        prefix,
        RostraId::assemble(prefix, first_rest),
        RostraId::assemble(prefix, second_rest),
    )
}

fn post(
    signing_secret: RostraIdSecretKey,
    author: RostraId,
    text: &str,
    timestamp: i64,
) -> VerifiedEventContent {
    let content = content_kind::SocialPost::new(text.to_owned(), None, Default::default())
        .serialize_cbor()
        .expect("social post must serialize");
    let event = Event::builder_raw_content()
        .author(signing_secret.id())
        .kind(EventKind::SOCIAL_POST)
        .content(&content)
        .timestamp(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(timestamp))
        .build();
    let signed = event.signed_by(signing_secret);
    let mut event = VerifiedEvent::verify_signed(signing_secret.id(), signed)
        .expect("fixture signature must verify before author substitution");

    // Producing two real Ed25519 identities with the same 128-bit prefix is
    // infeasible. The ingestion boundary accepts VerifiedEvent, so this fixture
    // substitutes its author after verification to exercise that boundary's
    // collision handling without weakening production verification.
    event.event.author = author;
    event.event_id = event.event.compute_id();

    VerifiedEventContent::assume_verified(event, content)
}

async fn ingest(db: &Database, content: &VerifiedEventContent) -> DbResult<InsertEventOutcome> {
    db.try_process_event_with_content(content)
        .await
        .map(|(outcome, _)| outcome)
}

fn assert_collision(
    err: DbError,
    prefix: ShortRostraId,
    existing_id: RostraId,
    incoming_id: RostraId,
) {
    match err {
        DbError::IdentityPrefixCollision {
            prefix: actual_prefix,
            existing_id: actual_existing,
            incoming_id: actual_incoming,
            ..
        } => {
            assert_eq!(actual_prefix, prefix);
            assert_eq!(actual_existing, existing_id);
            assert_eq!(actual_incoming, incoming_id);
        }
        other => panic!("expected identity-prefix collision, got {other:?}"),
    }
}

async fn snapshot(
    db: &Database,
    prefix: ShortRostraId,
    accepted_event: ShortEventId,
    rejected_event: ShortEventId,
    accepted_content: ContentHash,
    rejected_content: ContentHash,
) -> DbResult<IngestionSnapshot> {
    db.read_with(|tx| {
        let events_table = tx.open_table(&events::TABLE)?;
        let content_table = tx.open_table(&content_store::TABLE)?;
        let posts = tx.open_table(&social_posts::TABLE)?;

        Ok(IngestionSnapshot {
            identity_rest: ids_full::get(tx, prefix)?,
            event_count: events_table.range(..)?.count(),
            events_by_time_count: tx.open_table(&events_by_time::TABLE)?.range(..)?.count(),
            missing_parent_count: tx.open_table(&events_missing::TABLE)?.range(..)?.count(),
            head_count: tx.open_table(&events_heads::TABLE)?.range(..)?.count(),
            content_count: content_table.range(..)?.count(),
            content_state_count: tx
                .open_table(&events_content_state::TABLE)?
                .range(..)?
                .count(),
            content_queue_count: tx
                .open_table(&events_content_missing::TABLE)?
                .range(..)?
                .count(),
            content_rc_count: tx.open_table(&content_rc::TABLE)?.range(..)?.count(),
            usage_count: tx.open_table(&ids_data_usage::TABLE)?.range(..)?.count(),
            reception_count: tx
                .open_table(&events_received_at::TABLE)?
                .range(..)?
                .count(),
            post_count: posts.range(..)?.count(),
            post_time_count: tx
                .open_table(&social_posts_by_time::TABLE)?
                .range(..)?
                .count(),
            accepted_event_present: events_table.get(&accepted_event)?.is_some(),
            rejected_event_present: events_table.get(&rejected_event)?.is_some(),
            accepted_content_present: content_table.get(&accepted_content)?.is_some(),
            rejected_content_present: content_table.get(&rejected_content)?.is_some(),
        })
    })
    .await
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn identity_prefix_collision_preserves_first_mapping_and_is_atomic() -> BoxedErrorResult<()> {
    let self_id = RostraIdSecretKey::from_bytes([43; 32]).id();
    let (prefix, first_id, second_id) = colliding_ids();
    let first = post(
        RostraIdSecretKey::from_bytes([41; 32]),
        first_id,
        "first identity",
        41,
    );
    let second = post(
        RostraIdSecretKey::from_bytes([42; 32]),
        second_id,
        "second identity",
        42,
    );
    let first_followup = post(
        RostraIdSecretKey::from_bytes([41; 32]),
        first_id,
        "first identity followup",
        43,
    );
    let second_followup = post(
        RostraIdSecretKey::from_bytes([42; 32]),
        second_id,
        "second identity followup",
        44,
    );

    for (accepted, same_identity, rejected) in [
        (&first, &first_followup, &second),
        (&second, &second_followup, &first),
    ] {
        let dir = tempdir()?;
        let path = dir.path().join("db.redb");
        let db = Database::open(&path, self_id).await?;
        let accepted_id = accepted.event_id().to_short();
        let rejected_id = rejected.event_id().to_short();
        let accepted_author = accepted.author();
        let rejected_author = rejected.author();

        assert!(matches!(
            ingest(&db, accepted).await?,
            InsertEventOutcome::Inserted { .. }
        ));
        assert!(matches!(
            ingest(&db, same_identity).await?,
            InsertEventOutcome::Inserted { .. }
        ));
        assert!(matches!(
            ingest(&db, accepted).await?,
            InsertEventOutcome::AlreadyPresent
        ));

        let before_collision = snapshot(
            &db,
            prefix,
            accepted_id,
            rejected_id,
            accepted.content_hash(),
            rejected.content_hash(),
        )
        .await?;
        assert_eq!(
            before_collision.identity_rest,
            Some(accepted_author.split().1)
        );
        assert!(before_collision.accepted_event_present);
        assert!(before_collision.accepted_content_present);
        assert_eq!(before_collision.event_count, 2);
        assert_eq!(before_collision.post_time_count, 2);

        assert_collision(
            ingest(&db, rejected)
                .await
                .expect_err("different identity remainder must fail"),
            prefix,
            accepted_author,
            rejected_author,
        );
        assert_eq!(
            snapshot(
                &db,
                prefix,
                accepted_id,
                rejected_id,
                accepted.content_hash(),
                rejected.content_hash(),
            )
            .await?,
            before_collision,
            "collision changed envelope, content, lifecycle, or projection state"
        );

        assert!(
            AssertUnwindSafe(db.process_event_with_content(rejected))
                .catch_unwind()
                .await
                .is_err(),
            "compatibility wrapper must panic after collision rollback"
        );
        assert_eq!(
            snapshot(
                &db,
                prefix,
                accepted_id,
                rejected_id,
                accepted.content_hash(),
                rejected.content_hash(),
            )
            .await?,
            before_collision,
            "panicking compatibility wrapper committed collision effects"
        );

        drop(db);
        let reopened = Database::open(&path, self_id).await?;
        assert_eq!(
            snapshot(
                &reopened,
                prefix,
                accepted_id,
                rejected_id,
                accepted.content_hash(),
                rejected.content_hash(),
            )
            .await?,
            before_collision,
            "reopen changed the surviving first mapping"
        );
        assert_collision(
            ingest(&reopened, rejected)
                .await
                .expect_err("collision must remain rejected after reopen"),
            prefix,
            accepted_author,
            rejected_author,
        );
        assert_eq!(
            snapshot(
                &reopened,
                prefix,
                accepted_id,
                rejected_id,
                accepted.content_hash(),
                rejected.content_hash(),
            )
            .await?,
            before_collision
        );
    }

    Ok(())
}

async fn prepare_total_replay(path: &std::path::Path) -> BoxedErrorResult<()> {
    let inner = redb_bincode::Database::from(redb::Database::open(path)?);
    Database::write_with_inner(&inner, |tx| {
        tx.open_table(&db_version::TABLE)?.insert(&(), &24)?;
        Ok(())
    })
    .await?;
    Database::write_with_inner(&inner, Database::handle_db_ver_migrations).await?;
    Ok(())
}

async fn seed_identity(path: &std::path::Path, id: Option<RostraId>) -> BoxedErrorResult<()> {
    let inner = redb_bincode::Database::from(redb::Database::open(path)?);
    Database::write_with_inner(&inner, |tx| {
        if let Some(id) = id {
            let (prefix, rest) = id.split();
            ids_full::set_for_test(tx, prefix, Some(rest))?;
        } else {
            let (prefix, _, _) = colliding_ids();
            ids_full::set_for_test(tx, prefix, None)?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

async fn assert_replay_still_pending(
    path: &std::path::Path,
    colliding_id: RostraId,
    preceding_author: RostraId,
) -> BoxedErrorResult<()> {
    let inner = redb_bincode::Database::from(redb::Database::open(path)?);
    assert!(Database::write_with_inner(&inner, Database::has_pending_migration_stash).await?);
    Database::read_with_inner(&inner, |tx| {
        assert_eq!(
            ids_full::get(tx, colliding_id.split().0)?,
            Some(colliding_id.split().1)
        );
        assert_eq!(
            ids_full::get(tx, preceding_author.split().0)?,
            None,
            "preceding replay identity registration committed before collision"
        );
        assert_eq!(tx.open_table(&events::TABLE)?.range(..)?.count(), 0);
        assert_eq!(tx.open_table(&content_store::TABLE)?.range(..)?.count(), 0);
        assert_eq!(
            tx.open_table(&events_content_state::TABLE)?
                .range(..)?
                .count(),
            0
        );
        assert_eq!(tx.open_table(&social_posts::TABLE)?.range(..)?.count(), 0);
        assert_eq!(
            tx.open_table(&social_posts_by_time::TABLE)?
                .range(..)?
                .count(),
            0
        );
        Ok(())
    })
    .await?;
    Ok(())
}

fn post_ordered_before(target: ShortEventId) -> VerifiedEventContent {
    let secret = RostraIdSecretKey::from_bytes([44; 32]);
    for nonce in 0..256 {
        let candidate = post(
            secret,
            secret.id(),
            &format!("earlier replay source {nonce}"),
            50,
        );
        if candidate.event_id().to_short() < target {
            return candidate;
        }
    }
    panic!("failed to find deterministic lower event ID fixture");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn total_replay_collision_fails_deterministically_and_remains_retryable()
-> BoxedErrorResult<()> {
    let dir = tempdir()?;
    let path = dir.path().join("db.redb");
    let secret = RostraIdSecretKey::from_bytes([41; 32]);
    let author = secret.id();
    let (_, _, colliding_id) = colliding_ids();
    let content = post(secret, author, "retained replay source", 51);
    let preceding = post_ordered_before(content.event_id().to_short());

    let db = Database::open(&path, author).await?;
    ingest(&db, &preceding).await?;
    ingest(&db, &content).await?;
    drop(db);

    prepare_total_replay(&path).await?;
    seed_identity(&path, Some(colliding_id)).await?;

    for _ in 0..2 {
        let err = match Database::open(&path, author).await {
            Ok(_) => panic!("replay unexpectedly accepted an identity collision"),
            Err(err) => err,
        };
        assert_collision(err, author.split().0, colliding_id, author);
        assert_replay_still_pending(&path, colliding_id, preceding.author()).await?;
    }

    seed_identity(&path, None).await?;
    let replayed = Database::open(&path, author).await?;
    let event_id = content.event_id().to_short();
    let restored = snapshot(
        &replayed,
        author.split().0,
        event_id,
        ShortEventId::MAX,
        content.content_hash(),
        ContentHash::MAX,
    )
    .await?;
    assert!(restored.accepted_event_present);
    assert!(restored.accepted_content_present);
    assert_eq!(restored.event_count, 2);
    assert_eq!(restored.content_count, 2);
    assert_eq!(restored.post_time_count, 2);

    Ok(())
}
