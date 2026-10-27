use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;

use rostra_core::event::{
    Event, EventContentKind as _, EventContentRaw, EventExt as _, EventKind, SocialPost,
    VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, ExternalEventId, Timestamp};

use crate::news_post_mutation::{
    NewsPostMutation, NewsPostMutationRequest, NewsPostMutationRequestId,
};
use crate::social_post_mutation::{
    SocialPostMutation, SocialPostMutationRequest, SocialPostMutationRequestId,
};
use crate::{Database, DbError, DbResult, EventContentAvailability};

fn news(body: Option<&str>) -> SocialPost {
    let mut post = SocialPost::new_text(body.unwrap_or_default().into(), None, BTreeSet::new())
        .with_news_fields(
            Some("https://example.org/a?q=b#c".parse().unwrap()),
            Some(" Exact title ".into()),
        );
    post.djot_content = body.map(str::to_owned);
    post
}

fn raw_signed(
    secret: RostraIdSecretKey,
    raw: EventContentRaw,
    replace: Option<EventId>,
    time: u64,
) -> VerifiedEventContent {
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(Timestamp::from(time).to_offset_date_time().unwrap())
        .maybe_delete(replace.map(|id| id.to_short()))
        .content(&raw)
        .build()
        .signed_by(secret);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), signed).unwrap(),
        raw,
    )
    .unwrap()
}

fn signed(
    secret: RostraIdSecretKey,
    post: &SocialPost,
    replace: Option<EventId>,
    time: u64,
) -> VerifiedEventContent {
    raw_signed(secret, post.serialize_cbor().unwrap(), replace, time)
}

async fn ingest(db: &Database, content: &VerifiedEventContent) -> DbResult<()> {
    db.write_with(|tx| {
        db.process_event_tx(&content.event, Timestamp::from(100), tx)?;
        db.process_event_content_tx(content, Timestamp::from(100), tx)
    })
    .await
}

fn request(target: EventId, body: Option<&str>) -> NewsPostMutationRequest {
    NewsPostMutationRequest {
        expected_current: target,
        mutation: body.map_or(NewsPostMutation::Delete, |body| NewsPostMutation::Edit {
            body: body.into(),
        }),
    }
}

async fn commit(
    db: &Database,
    id: u8,
    request: &NewsPostMutationRequest,
    candidate: &VerifiedEventContent,
) -> DbResult<VerifiedEvent> {
    db.commit_news_post_mutation(
        NewsPostMutationRequestId([id; 16]),
        request,
        candidate,
        &mut None,
        &mut None,
    )
    .await
}

#[test]
fn news_mutation_digest_v1_is_independent_of_legacy_text() {
    let account = rostra_core::id::RostraId::from_bytes([3; 32]);
    let target = EventId::from_bytes([5; 32]);
    let input = request(target, Some("hello\0世界\n"));
    assert_eq!(
        input.digest(account),
        [
            27, 231, 61, 247, 225, 131, 233, 89, 211, 219, 164, 228, 108, 158, 33, 182, 34, 140,
            146, 130, 242, 230, 92, 43, 226, 23, 228, 197, 165, 170, 61, 85,
        ]
    );
    assert_ne!(
        input.digest(account),
        SocialPostMutationRequest {
            expected_current: target,
            mutation: SocialPostMutation::Edit {
                body: "hello\0世界\n".into()
            },
        }
        .digest(account)
    );
    assert_ne!(input.digest(account), request(target, None).digest(account));
}

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_title_only_metadata_and_genuine_delete() -> anyhow::Result<()> {
    for body in [None, Some(""), Some(" \n"), Some("original")] {
        let secret = RostraIdSecretKey::generate();
        let db = Database::new_in_memory(secret.id()).await?;
        // Keep a historical persona independently of its empty declared tag set.
        let mut original_post =
            SocialPost::decode_mutable_news(b"\xa3\x61p\x02\x61n\xf5\x61t\x80").unwrap();
        let fields = news(body);
        original_post.djot_content = fields.djot_content;
        original_post.title = fields.title;
        original_post.url = fields.url;
        let original = signed(secret, &original_post, None, 1);
        ingest(&db, &original).await?;
        assert_eq!(
            db.prepare_news_post_mutation(original.event_id()).await?,
            original_post
        );
        assert!(matches!(
            db.prepare_social_post_mutation(original.event_id()).await,
            Err(DbError::SocialPostMutationUnsupported)
        ));
        let edit_request = request(original.event_id(), Some(" ❤️ \n"));
        let mut edited = original_post.clone();
        edited.djot_content = Some(" ❤️ \n".into());
        let edit = signed(secret, &edited, Some(original.event_id()), 2);
        commit(&db, 1, &edit_request, &edit).await?;
        assert_eq!(
            db.get_social_post(edit.event_id().to_short())
                .await
                .unwrap()
                .content,
            edited
        );
        let deletion_request = request(edit.event_id(), None);
        // Merely blanking the news body is not the supported Delete payload.
        let mut wrong_delete = edited.clone();
        wrong_delete.djot_content = Some(String::new());
        let wrong = signed(secret, &wrong_delete, Some(edit.event_id()), 3);
        assert!(matches!(
            commit(&db, 2, &deletion_request, &wrong).await,
            Err(DbError::SocialPostCandidate)
        ));
        assert!(db.get_event(wrong.event_id()).await.is_none());
        let plain = SocialPost::new_text(String::new(), None, Default::default());
        let deletion = signed(secret, &plain, Some(edit.event_id()), 4);
        commit(&db, 2, &deletion_request, &deletion).await?;
        let state = db
            .get_social_post_state(original.event_id().to_short())
            .await
            .unwrap();
        assert_eq!(state.event_id, edit.event_id().to_short());
        assert_eq!(state.availability, EventContentAvailability::Deleted);
        assert!(state.post.is_none());
        assert!(
            db.get_social_post_state(deletion.event_id().to_short())
                .await
                .unwrap()
                .is_tombstone
        );
        assert!(db.paginate_news_ranked(None, 10).await?.rows.is_empty());
        // Receipt resolution ignores the stale target and an unrelated candidate.
        assert_eq!(
            commit(&db, 1, &edit_request, &wrong).await?.event_id,
            edit.event_id()
        );
        assert_eq!(
            commit(&db, 2, &deletion_request, &wrong).await?.event_id,
            deletion.event_id()
        );
        assert!(matches!(
            commit(&db, 1, &request(original.event_id(), Some(" ")), &wrong).await,
            Err(DbError::NewsPostMutationMismatch)
        ));
        // Original title-only news can also be deleted without first adding text.
        let title_only = signed(secret, &news(None), None, 5);
        ingest(&db, &title_only).await?;
        let deletion = signed(secret, &plain, Some(title_only.event_id()), 6);
        commit(&db, 3, &request(title_only.event_id(), None), &deletion).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_reply_lineage_counters_and_vote_rank_generation() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let parent = signed(secret, &news(Some("parent")), None, 1);
    ingest(&db, &parent).await?;
    let mut reply_post = news(None);
    reply_post.reply_to = Some(ExternalEventId::new(secret.id(), parent.event_id()));
    let reply = signed(secret, &reply_post, None, 2);
    ingest(&db, &reply).await?;
    let child_post = SocialPost::new_text(
        "child".into(),
        Some(ExternalEventId::new(secret.id(), reply.event_id())),
        Default::default(),
    );
    let child = signed(secret, &child_post, None, 3);
    ingest(&db, &child).await?;
    // Historical absent-body replies were not counted; do not reinterpret them.
    assert_eq!(
        db.get_social_post(parent.event_id().to_short())
            .await
            .unwrap()
            .reply_count,
        0
    );
    let input = request(reply.event_id(), Some("reply corrected"));
    let edit = signed(secret, &input.apply(reply_post), Some(reply.event_id()), 4);
    commit(&db, 1, &input, &edit).await?;
    let state = db
        .get_social_post_state(reply.event_id().to_short())
        .await
        .unwrap();
    let post = state.post.unwrap();
    assert_eq!(state.event_id, edit.event_id().to_short());
    assert_eq!(
        post.reply_to,
        Some(ExternalEventId::new(secret.id(), parent.event_id()))
    );
    assert_eq!(post.reply_count, 1);
    assert_eq!(
        db.get_social_post(parent.event_id().to_short())
            .await
            .unwrap()
            .reply_count,
        1
    );
    let delete = request(edit.event_id(), None);
    let deletion = signed(secret, &delete.apply(news(None)), Some(edit.event_id()), 5);
    commit(&db, 2, &delete, &deletion).await?;
    assert_eq!(
        db.get_social_post(parent.event_id().to_short())
            .await
            .unwrap()
            .reply_count,
        0
    );
    assert!(
        db.get_social_post(child.event_id().to_short())
            .await
            .is_some()
    );
    let state = db
        .get_social_post_state(reply.event_id().to_short())
        .await
        .unwrap();
    assert_eq!(state.event_id, edit.event_id().to_short());
    assert_eq!(state.availability, EventContentAvailability::Deleted);

    // Voting stays exact-event scoped; editing never transfers a historical vote.
    let another = signed(secret, &news(Some("other")), None, 6);
    ingest(&db, &another).await?;
    let vote = crate::vote_request::VoteRequest {
        target: (secret.id(), parent.event_id()),
        value: Some(true),
    };
    let vote_raw = vote.to_social_vote().serialize_cbor()?;
    let vote_event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_VOTE)
        .timestamp(Timestamp::from(7).to_offset_date_time().unwrap())
        .maybe_singleton_aux_key(vote.to_social_vote().singleton_key_aux())
        .content(&vote_raw)
        .build()
        .signed_by(secret);
    let vote_content = VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), vote_event)?,
        vote_raw,
    )?;
    db.commit_vote(
        crate::vote_request::VoteRequestId([1; 16]),
        &vote,
        &vote_content,
        &mut None,
        &mut None,
    )
    .await?;
    let page = db.paginate_news_ranked(None, 1).await?;
    let cursor = page.next_cursor.expect("another ranked row");
    let input = request(parent.event_id(), Some("parent corrected"));
    let edit = signed(
        secret,
        &input.apply(news(Some("parent"))),
        Some(parent.event_id()),
        8,
    );
    commit(&db, 3, &input, &edit).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    let rows = db.paginate_news_ranked(None, 100).await?.rows;
    let row = rows
        .iter()
        .find(|row| row.target.1 == edit.event_id())
        .unwrap();
    assert_eq!((row.self_vote, row.post.vote_sum), (None, 0));
    assert_eq!(
        db.get_social_vote(
            secret.id(),
            ExternalEventId::new(secret.id(), parent.event_id()),
        )
        .await,
        Some(Some(true))
    );
    assert_eq!(
        db.get_social_vote(
            secret.id(),
            ExternalEventId::new(secret.id(), edit.event_id()),
        )
        .await,
        None
    );
    assert!(!rows.iter().any(|row| row.target.1 == parent.event_id()));
    assert_eq!(
        db.vote_receipt(crate::vote_request::VoteRequestId([1; 16]), &vote)
            .await?
            .unwrap()
            .event_id,
        vote_content.event_id()
    );
    let cursor = db.paginate_news_ranked(None, 1).await?.next_cursor.unwrap();
    let delete = request(edit.event_id(), None);
    let deletion = signed(secret, &delete.apply(news(None)), Some(edit.event_id()), 9);
    commit(&db, 4, &delete, &deletion).await?;
    assert!(matches!(
        db.paginate_news_ranked(Some(cursor), 1).await,
        Err(DbError::NewsCursorStale)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_strict_targets_candidates_and_canonical_forks() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let post = news(Some("original"));
    let original = signed(secret, &post, None, 1);
    ingest(&db, &original).await?;
    let mut spoof = original.event_id().to_bytes();
    spoof[31] ^= 1;
    assert!(matches!(
        db.prepare_news_post_mutation(EventId::from_bytes(spoof))
            .await,
        Err(DbError::SocialPostMutationTargetMismatch)
    ));
    let foreign = signed(RostraIdSecretKey::generate(), &post, None, 1);
    ingest(&db, &foreign).await?;
    assert!(matches!(
        db.prepare_news_post_mutation(foreign.event_id()).await,
        Err(DbError::SocialPostMutationNotOwner)
    ));
    for bytes in [
        b"\xa3\x61n\xf5\x61c\x61x\x61z\x01".as_slice(), // unknown
        b"\xa3\x61n\xf5\x61n\xf5\x61c\x61x",            // duplicate
        b"\xa2\x61n\xf5\x61c\x61x\x00",                 // trailing
        b"\xa2\x61n\x01\x61c\x61x",                     // wrong typed flag
        b"\xa2\x61n\xf5\x61e\x61x",                     // reaction
        b"\xa1\x61c\x61x",                              // text is not news
    ] {
        assert!(SocialPost::decode_mutable_news(bytes).is_none());
    }
    let unknown = raw_signed(
        secret,
        EventContentRaw::new(b"\xa3\x61n\xf5\x61c\x61x\x61z\x01".to_vec()),
        None,
        2,
    );
    ingest(&db, &unknown).await?;
    assert!(matches!(
        db.prepare_news_post_mutation(unknown.event_id()).await,
        Err(DbError::SocialPostMutationUnsupported)
    ));
    let blank_deleting_news = signed(secret, &news(None), Some(unknown.event_id()), 3);
    ingest(&db, &blank_deleting_news).await?;
    assert!(matches!(
        db.prepare_news_post_mutation(blank_deleting_news.event_id())
            .await,
        Err(DbError::SocialPostMutationDeleted)
    ));
    assert!(
        !db.get_social_post_state(blank_deleting_news.event_id().to_short())
            .await
            .unwrap()
            .is_tombstone
    );
    let input = request(original.event_id(), Some("exact edit"));
    let mut wrong_post = input.apply(post.clone());
    wrong_post.title = Some("changed title".into());
    let wrong = signed(secret, &wrong_post, Some(original.event_id()), 4);
    assert!(matches!(
        commit(&db, 1, &input, &wrong).await,
        Err(DbError::SocialPostCandidate)
    ));
    assert!(db.get_event(wrong.event_id()).await.is_none());
    for body in ["", " \n"] {
        assert!(matches!(
            commit(&db, 1, &request(original.event_id(), Some(body)), &wrong).await,
            Err(DbError::SocialPostMutationEmpty)
        ));
    }
    let a = signed(
        secret,
        &input.apply(post.clone()),
        Some(original.event_id()),
        5,
    );
    let b = signed(secret, &news(Some("fork")), Some(original.event_id()), 6);
    ingest(&db, &a).await?;
    ingest(&db, &b).await?;
    let (winner, loser) = if a.event_id().to_short() < b.event_id().to_short() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    db.prepare_news_post_mutation(winner.event_id()).await?;
    assert!(matches!(
        db.prepare_news_post_mutation(loser.event_id()).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    // A leaf on a noncanonical sibling is still stale.
    let leaf = signed(
        secret,
        &news(Some("losing leaf")),
        Some(loser.event_id()),
        7,
    );
    ingest(&db, &leaf).await?;
    assert!(matches!(
        db.prepare_news_post_mutation(leaf.event_id()).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    assert!(matches!(
        commit(&db, 1, &input, &wrong).await,
        Err(DbError::SocialPostMutationConflict)
    ));
    assert!(db.get_event(wrong.event_id()).await.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_concurrent_same_and_different_keys() -> anyhow::Result<()> {
    for same_key in [true, false] {
        let secret = RostraIdSecretKey::generate();
        let db = Arc::new(Database::new_in_memory(secret.id()).await?);
        let original_post = news(None);
        let original = signed(secret, &original_post, None, 1);
        ingest(&db, &original).await?;
        let mut tasks = Vec::new();
        for index in 0..8u8 {
            let db = db.clone();
            let post = original_post.clone();
            let target = original.event_id();
            tasks.push(tokio::spawn(async move {
                let input = request(target, (same_key || index % 2 != 0).then_some("exact edit"));
                let candidate = signed(
                    secret,
                    &input.apply(post),
                    Some(target),
                    2 + u64::from(index),
                );
                let result =
                    commit(&db, if same_key { 1 } else { index }, &input, &candidate).await;
                (result, candidate.event_id())
            }));
        }
        let mut results = BTreeSet::new();
        let mut candidates = BTreeSet::new();
        let mut successes = 0;
        for task in tasks {
            let (result, candidate) = task.await?;
            candidates.insert(candidate);
            match result {
                Ok(event) => {
                    successes += 1;
                    results.insert(event.event_id);
                }
                Err(DbError::SocialPostMutationConflict | DbError::SocialPostMutationDeleted)
                    if !same_key => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(results.len(), 1);
        assert_eq!(successes, if same_key { 8 } else { 1 });
        assert_eq!(candidates.len(), 8);
        for candidate in candidates {
            assert_eq!(
                db.get_event(candidate).await.is_some(),
                results.contains(&candidate)
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_upgrade_rollback_rebuild_and_legacy_receipts() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("mutations.redb");
    let db = Database::open(&path, secret.id()).await?;
    let text = SocialPost::new_text("text".into(), None, Default::default());
    let original_text = signed(secret, &text, None, 1);
    ingest(&db, &original_text).await?;
    let legacy_request = SocialPostMutationRequest {
        expected_current: original_text.event_id(),
        mutation: SocialPostMutation::Delete,
    };
    let plain = SocialPost::new_text(String::new(), None, Default::default());
    let legacy_delete = signed(secret, &plain, Some(original_text.event_id()), 2);
    db.commit_social_post_mutation(
        SocialPostMutationRequestId([1; 16]),
        &legacy_request,
        &legacy_delete,
        &mut None,
        &mut None,
    )
    .await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                "social_news_mutation_receipts",
            ))?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &41)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    let original = signed(secret, &news(None), None, 3);
    ingest(&db, &original).await?;
    let input = request(original.event_id(), None);
    let candidate = signed(secret, &plain, Some(original.event_id()), 4);
    let failure = db
        .commit_news_post_mutation_inner(
            NewsPostMutationRequestId([1; 16]),
            &input,
            &candidate,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::SocialPostCandidate))),
        )
        .await;
    assert!(matches!(failure, Err(DbError::SocialPostCandidate)));
    assert!(db.get_event(candidate.event_id()).await.is_none());
    assert!(
        db.news_post_mutation_receipt(NewsPostMutationRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
    db.prepare_news_post_mutation(original.event_id()).await?;
    assert_eq!(db.paginate_news_ranked(None, 10).await?.rows.len(), 1);
    commit(&db, 1, &input, &candidate).await?;
    // Remove the tombstone's body; retry must not restore it.
    db.write_with(|tx| {
        let before = db.payload_before_tx(tx, &candidate.event)?;
        Database::prune_event_content_tx(
            candidate.event_id(),
            candidate.content_hash(),
            &mut tx.open_table(&crate::events_content_state::TABLE)?,
            &mut tx.open_table(&crate::content_rc::TABLE)?,
            &mut tx.open_table(&crate::events_content_missing::TABLE)?,
            Some((
                secret.id(),
                candidate.content_len(),
                &mut tx.open_table(&crate::ids_data_usage::TABLE)?,
            )),
        )?;
        db.payload_after_tx(tx, before)
    })
    .await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate).await?.event_id,
        candidate.event_id()
    );
    assert!(matches!(
        db.get_event_content_availability(candidate.event_id())
            .await,
        Some(EventContentAvailability::Pruned { .. })
    ));
    // Durable deletion, unlike manual pruning, is authoritative across replay.
    let delete_tombstone = signed(secret, &plain, Some(candidate.event_id()), 5);
    ingest(&db, &delete_tombstone).await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 42))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &candidate).await?.event_id,
        candidate.event_id()
    );
    assert!(matches!(
        db.get_event_content_availability(candidate.event_id())
            .await,
        Some(EventContentAvailability::Deleted)
    ));
    assert_eq!(
        db.social_post_mutation_receipt(SocialPostMutationRequestId([1; 16]), &legacy_request)
            .await?
            .unwrap()
            .event_id,
        legacy_delete.event_id()
    );
    assert!(db.paginate_news_ranked(None, 10).await?.rows.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn news_mutation_admission_lost_ack_and_edit_receipt_replay() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("admission.redb");
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    let original = signed(secret, &news(None), None, 1);
    ingest(&db, &original).await?;
    let input = request(original.event_id(), Some("exact correction"));
    let candidate = signed(
        secret,
        &input.apply(news(None)),
        Some(original.event_id()),
        2,
    );
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    let configure = |limit| {
        crate::PayloadAdmissionConfig::new(crate::PayloadAdmissionLimits {
            database_bytes: NonZeroU64::new(limit).unwrap(),
            author_bytes: NonZeroU64::new(limit).unwrap(),
            overrides: Default::default(),
            in_flight_count: NonZeroUsize::new(4).unwrap(),
            in_flight_bytes: NonZeroU64::new(10_000).unwrap(),
        })
        .unwrap()
    };
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(configure(1));
        Ok(())
    })
    .await?;
    assert!(matches!(
        commit(&db, 1, &input, &candidate).await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(db.get_event(candidate.event_id()).await.is_none());
    assert!(
        db.news_post_mutation_receipt(NewsPostMutationRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
    db.prepare_news_post_mutation(original.event_id()).await?;
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(configure(10_000));
        Ok(())
    })
    .await?;
    let task = tokio::spawn({
        let db = db.clone();
        let input = input.clone();
        let candidate = candidate.clone();
        async move {
            let mut allocation = db
                .reserve_payload_allocation(u64::from(candidate.content_len()))
                .unwrap();
            db.commit_news_post_mutation_inner(
                NewsPostMutationRequestId([1; 16]),
                &input,
                &candidate,
                &mut allocation,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("lost acknowledgment after commit"));
                    Ok(())
                })),
            )
            .await
        }
    });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(db.payload_admission_usage().buffers, 0);
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = None;
        Ok(())
    })
    .await?;
    let delete_request = request(candidate.event_id(), None);
    let deletion = signed(
        secret,
        &delete_request.apply(news(None)),
        Some(candidate.event_id()),
        3,
    );
    commit(&db, 2, &delete_request, &deletion).await?;
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(configure(1));
        Ok(())
    })
    .await?;
    // Admission is now paused and both target bodies are deleted; receipt wins.
    assert_eq!(
        commit(&db, 1, &input, &deletion).await?.event_id,
        candidate.event_id()
    );
    db.write_with(|tx| Database::prepare_total_migration(tx, 42))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, 1, &input, &deletion).await?.event_id,
        candidate.event_id()
    );
    let state = db
        .get_social_post_state(original.event_id().to_short())
        .await
        .unwrap();
    assert_eq!(state.event_id, candidate.event_id().to_short());
    assert_eq!(state.availability, EventContentAvailability::Deleted);
    assert!(db.paginate_news_ranked(None, 10).await?.rows.is_empty());
    Ok(())
}
