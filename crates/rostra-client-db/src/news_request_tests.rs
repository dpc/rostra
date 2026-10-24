use rostra_core::event::{Event, EventExt as _, SocialPost, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, Timestamp};

use crate::news_post_request::{NewsPostRequest, NewsPostRequestId};
use crate::vote_request::{VoteRequest, VoteRequestId};
use crate::{Database, DbError, DbResult};

#[test]
fn news_vote_v1_digest_vectors() {
    let account = rostra_core::id::RostraId::from_bytes([3; 32]);
    let news = NewsPostRequest {
        body: "body".into(),
        title: "Title".into(),
        url: Some("https://EXAMPLE.org".into()),
    };
    let vote = VoteRequest {
        target: (
            rostra_core::id::RostraId::from_bytes([4; 32]),
            EventId::from_bytes([5; 32]),
        ),
        value: None,
    };
    assert_eq!(
        news.digest(account),
        [
            12, 103, 145, 243, 227, 79, 158, 172, 0, 43, 118, 109, 24, 205, 48, 51, 234, 245, 27,
            150, 138, 7, 115, 0, 103, 146, 252, 36, 105, 67, 177, 26
        ]
    );
    assert_eq!(
        vote.digest(account),
        [
            165, 140, 164, 40, 8, 132, 154, 195, 117, 3, 169, 100, 255, 168, 204, 236, 32, 194, 95,
            57, 180, 166, 144, 243, 123, 150, 35, 220, 160, 175, 118, 216
        ]
    );
    assert_eq!(
        NewsPostRequestId([6; 16]).event_aux_key(account).to_bytes(),
        [
            184, 22, 14, 100, 255, 155, 216, 72, 51, 165, 121, 33, 207, 69, 61, 158
        ]
    );
}

pub(crate) fn input(body: impl Into<String>) -> NewsPostRequest {
    NewsPostRequest {
        body: body.into(),
        title: "News".into(),
        url: Some("https://example.org/".into()),
    }
}

pub(crate) fn signed<C: rostra_core::event::EventContentKind>(
    secret: RostraIdSecretKey,
    payload: &C,
    time: u64,
    delete: Option<EventId>,
) -> VerifiedEventContent {
    let raw = payload.serialize_cbor().unwrap();
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .kind(C::KIND)
        .maybe_singleton_aux_key(payload.singleton_key_aux())
        .maybe_delete(delete.map(|id| id.to_short()))
        .content(&raw)
        .build();
    event.timestamp = Timestamp::from(time).into();
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap(),
        raw,
    )
    .unwrap()
}

pub(crate) async fn ingest(db: &Database, content: &VerifiedEventContent) -> DbResult<()> {
    db.write_with(|tx| {
        db.process_event_tx(&content.event, Timestamp::from(100), tx)?;
        db.process_event_content_tx(content, Timestamp::from(100), tx)
    })
    .await
}

pub(crate) fn news(
    secret: RostraIdSecretKey,
    id: u8,
    request: &NewsPostRequest,
    time: u64,
) -> VerifiedEventContent {
    let mut content = signed(secret, &request.to_social_post().unwrap(), time, None);
    let mut event = content.event.event;
    event.key_aux = NewsPostRequestId([id; 16]).event_aux_key(secret.id());
    content.event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap();
    content
}

async fn commit_news(
    db: &Database,
    id: u8,
    request: &NewsPostRequest,
    candidate: &VerifiedEventContent,
) -> DbResult<VerifiedEvent> {
    db.commit_news_post(
        NewsPostRequestId([id; 16]),
        request,
        candidate,
        &mut None,
        &mut None,
    )
    .await
}

async fn commit_vote(
    db: &Database,
    id: u8,
    request: &VoteRequest,
    candidate: &VerifiedEventContent,
) -> DbResult<VerifiedEvent> {
    db.commit_vote(
        VoteRequestId([id; 16]),
        request,
        candidate,
        &mut None,
        &mut None,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn news_independence_retry_mismatch_and_exact_inputs() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let request = input("");
    let a = news(secret, 1, &request, 10);
    let b = news(secret, 2, &request, 10);
    assert_eq!(a.parent_prev(), b.parent_prev());
    assert_eq!(a.content_hash(), b.content_hash());
    assert_ne!(a.event_id(), b.event_id());
    let alternate = news(secret, 1, &request, 11);
    let (x, y) = tokio::join!(
        commit_news(&db, 1, &request, &a),
        commit_news(&db, 1, &request, &alternate)
    );
    let x = x?;
    assert_eq!(x.event_id, y?.event_id);
    commit_news(&db, 2, &request, &b).await?;
    assert_eq!(db.paginate_news_ranked(None, 100).await?.rows.len(), 2);
    assert!(matches!(
        commit_news(&db, 3, &request, &a).await,
        Err(DbError::NewsCandidate)
    ));
    for changed in [
        NewsPostRequest {
            body: " ".into(),
            ..request.clone()
        },
        NewsPostRequest {
            title: " News".into(),
            ..request.clone()
        },
        NewsPostRequest {
            url: Some("https://EXAMPLE.org/".into()),
            ..request.clone()
        },
        NewsPostRequest {
            url: None,
            ..request.clone()
        },
    ] {
        assert!(matches!(
            commit_news(&db, 1, &changed, &a).await,
            Err(DbError::NewsRequestMismatch)
        ));
    }
    let deletion = signed(
        secret,
        &SocialPost::new_text("".into(), None, Default::default()),
        20,
        Some(x.event_id),
    );
    ingest(&db, &deletion).await?;
    assert_eq!(
        commit_news(&db, 1, &request, &b).await?.event_id,
        x.event_id
    );
    assert!(db.get_event_content(x.event_id).await.is_none());
    let replica = Database::new_in_memory(secret.id()).await?;
    for item in [&b, &a] {
        ingest(&replica, item).await?;
    }
    assert_eq!(replica.paginate_news_ranked(None, 100).await?.rows.len(), 2);
    assert!(matches!(
        commit_news(&replica, 1, &request, &a).await,
        Err(DbError::NewsCandidate)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn votes_order_retry_old_after_new_exact_target_and_remote_replay() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let target = news(secret, 1, &input(""), 1);
    ingest(&db, &target).await?;
    let up = VoteRequest {
        target: (secret.id(), target.event_id()),
        value: Some(true),
    };
    let down = VoteRequest {
        value: Some(false),
        ..up.clone()
    };
    let clear = VoteRequest {
        value: None,
        ..up.clone()
    };
    let a = signed(secret, &up.to_social_vote(), 10, None);
    let tied = signed(secret, &down.to_social_vote(), 10, None);
    commit_vote(&db, 1, &up, &a).await?;
    // Distinct requests can prepare exactly the same candidate. Classify the
    // loser as ordering conflict so the public client waits/retries, not as
    // invalid input and not as a second accepted intent.
    assert!(matches!(
        commit_vote(&db, 9, &up, &a).await,
        Err(DbError::VoteOrderingConflict)
    ));
    assert!(
        db.vote_receipt(VoteRequestId([9; 16]), &up)
            .await?
            .is_none()
    );
    assert!(matches!(
        commit_vote(&db, 2, &down, &tied).await,
        Err(DbError::VoteOrderingConflict)
    ));
    assert!(db.get_event(tied.event_id()).await.is_none());
    assert!(
        db.vote_receipt(VoteRequestId([2; 16]), &down)
            .await?
            .is_none()
    );
    let b = signed(secret, &down.to_social_vote(), 11, None);
    commit_vote(&db, 2, &down, &b).await?;
    assert_eq!(commit_vote(&db, 1, &up, &b).await?.event_id, a.event_id());
    let row = &db.paginate_news_ranked(None, 1).await?.rows[0];
    assert_eq!((row.self_vote, row.post.vote_sum), (Some(false), -1));
    assert!(matches!(
        commit_vote(&db, 1, &clear, &a).await,
        Err(DbError::VoteRequestMismatch)
    ));
    let c = signed(secret, &clear.to_social_vote(), 12, None);
    commit_vote(&db, 3, &clear, &c).await?;
    for order in [[&a, &b, &c], [&c, &b, &a]] {
        let replica = Database::new_in_memory(secret.id()).await?;
        for item in order {
            ingest(&replica, item).await?;
        }
        ingest(&replica, &target).await?;
        let row = &replica.paginate_news_ranked(None, 1).await?.rows[0];
        assert_eq!((row.self_vote, row.post.vote_sum), (None, 0));
    }
    let future = signed(
        secret,
        &up.to_social_vote(),
        Timestamp::now().as_u64() + 100,
        None,
    );
    assert!(matches!(
        commit_vote(&db, 4, &up, &future).await,
        Err(DbError::VoteFutureTimestamp)
    ));
    // A later same-account device arrival can supersede locally accepted clear.
    ingest(&db, &future).await?;
    assert_eq!(
        db.paginate_news_ranked(None, 1).await?.rows[0].self_vote,
        Some(true)
    );
    assert_eq!(
        commit_vote(&db, 3, &clear, &c).await?.event_id,
        c.event_id()
    );
    assert_eq!(
        db.paginate_news_ranked(None, 1).await?.rows[0].self_vote,
        Some(true)
    );
    let mut wrong = up.clone();
    wrong.target.0 = RostraIdSecretKey::generate().id();
    assert!(matches!(
        commit_vote(&db, 8, &wrong, &a).await,
        Err(DbError::NewsTarget)
    ));
    let mut bytes = target.event_id().to_bytes();
    bytes[31] ^= 1;
    wrong = up.clone();
    wrong.target.1 = EventId::from_bytes(bytes);
    assert!(matches!(
        commit_vote(&db, 8, &wrong, &a).await,
        Err(DbError::NewsTarget)
    ));
    let deletion = signed(
        secret,
        &SocialPost::new_text("".into(), None, Default::default()),
        20,
        Some(target.event_id()),
    );
    ingest(&db, &deletion).await?;
    assert!(matches!(
        commit_vote(&db, 8, &clear, &c).await,
        Err(DbError::NewsUnavailable)
    ));
    assert_eq!(commit_vote(&db, 1, &up, &a).await?.event_id, a.event_id());
    assert_eq!(
        commit_vote(&db, 3, &clear, &c).await?.event_id,
        c.event_id()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn receipt_abort_postcommit_failure_upgrade_reopen_and_rebuild() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("news.redb");
    let db = Database::open(&path, secret.id()).await?;
    db.write_with(|tx| {
        for name in [
            "social_news_receipts",
            "social_vote_receipts",
            "social_news_generation",
        ] {
            tx.as_raw()
                .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(name))?;
        }
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &36)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = std::sync::Arc::new(Database::open(&path, secret.id()).await?);
    let request = input("body");
    let candidate = news(secret, 1, &request, 1);
    let failure = db
        .commit_news_post_inner(
            NewsPostRequestId([1; 16]),
            &request,
            &candidate,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::NewsCandidate))),
        )
        .await;
    assert!(failure.is_err());
    assert!(db.get_event(candidate.event_id()).await.is_none());
    assert!(
        db.news_post_receipt(NewsPostRequestId([1; 16]), &request)
            .await?
            .is_none()
    );
    assert!(db.paginate_news_ranked(None, 100).await?.rows.is_empty());
    let task = tokio::spawn({
        let db = db.clone();
        let request = request.clone();
        let candidate = candidate.clone();
        async move {
            db.commit_news_post_inner(
                NewsPostRequestId([1; 16]),
                &request,
                &candidate,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("ack lost"));
                    Ok(())
                })),
            )
            .await
        }
    });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(
        commit_news(&db, 1, &request, &candidate).await?.event_id,
        candidate.event_id()
    );
    let vote = VoteRequest {
        target: (secret.id(), candidate.event_id()),
        value: Some(true),
    };
    let signed_vote = signed(secret, &vote.to_social_vote(), 2, None);
    let failure = db
        .commit_vote_inner(
            VoteRequestId([1; 16]),
            &vote,
            &signed_vote,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::NewsCandidate))),
        )
        .await;
    assert!(failure.is_err());
    assert!(db.get_event(signed_vote.event_id()).await.is_none());
    assert!(
        db.vote_receipt(VoteRequestId([1; 16]), &vote)
            .await?
            .is_none()
    );
    assert_eq!(
        db.paginate_news_ranked(None, 1).await?.rows[0]
            .post
            .vote_sum,
        0
    );
    commit_vote(&db, 1, &vote, &signed_vote).await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 37))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit_news(&db, 1, &request, &candidate).await?.event_id,
        candidate.event_id()
    );
    assert_eq!(
        commit_vote(&db, 1, &vote, &signed_vote).await?.event_id,
        signed_vote.event_id()
    );
    assert_eq!(
        db.paginate_news_ranked(None, 1).await?.rows[0].self_vote,
        Some(true)
    );
    db.write_with(|tx| {
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &40)?;
        Ok(())
    })
    .await?;
    drop(db);
    assert!(matches!(
        Database::open(&path, secret.id()).await,
        Err(DbError::DbVersionTooHigh { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn vote_cancellation_after_commit_receipt_survives_reopen() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cancel.redb");
    let db = std::sync::Arc::new(Database::open(&path, secret.id()).await?);
    let target = news(secret, 1, &input("target"), 1);
    ingest(&db, &target).await?;
    let request = VoteRequest {
        target: (secret.id(), target.event_id()),
        value: None,
    };
    let content = signed(secret, &request.to_social_vote(), 2, None);
    let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn({
        let db = db.clone();
        let request = request.clone();
        let content = content.clone();
        async move {
            db.commit_vote_inner(
                VoteRequestId([1; 16]),
                &request,
                &content,
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
            .await?;
            std::future::pending::<()>().await;
            Ok::<(), DbError>(())
        }
    });
    committed_rx.await?;
    task.abort();
    release_tx.send(())?;
    assert!(task.await.unwrap_err().is_cancelled());
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit_vote(&db, 1, &request, &content).await?.event_id,
        content.event_id()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn news_admission_abort_and_protected_receipt_tables() -> anyhow::Result<()> {
    use std::num::{NonZeroU64, NonZeroUsize};

    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let request = input("x".repeat(1000));
    let content = news(secret, 1, &request, 1);
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    for max in [1, 10000] {
        db.payload_admission.state.lock().unwrap().config = Some(
            crate::PayloadAdmissionConfig::new(crate::PayloadAdmissionLimits {
                database_bytes: NonZeroU64::new(max).unwrap(),
                author_bytes: NonZeroU64::new(max).unwrap(),
                overrides: Default::default(),
                in_flight_count: NonZeroUsize::new(4).unwrap(),
                in_flight_bytes: NonZeroU64::new(10000).unwrap(),
            })
            .unwrap(),
        );
        let mut allocation = db.reserve_payload_allocation(u64::from(content.content_len()))?;
        let mut buffer = None;
        let result = db
            .commit_news_post_inner(
                NewsPostRequestId([1; 16]),
                &request,
                &content,
                &mut allocation,
                &mut buffer,
                Some(Box::new(|_| Err(DbError::NewsCandidate))),
            )
            .await;
        if max == 1 {
            assert!(matches!(
                result,
                Err(DbError::PayloadAdmissionPaused { .. })
            ));
        } else {
            assert!(matches!(result, Err(DbError::NewsCandidate)));
        }
        assert!(db.get_event(content.event_id()).await.is_none());
        assert!(
            db.news_post_receipt(NewsPostRequestId([1; 16]), &request)
                .await?
                .is_none()
        );
        drop(buffer);
        drop(allocation);
        assert_eq!(db.payload_admission_usage().acquisitions, 0);
        assert_eq!(db.payload_admission_usage().buffers, 0);
    }
    for name in [
        "social_news_receipts",
        "social_vote_receipts",
        "social_news_generation",
    ] {
        let table = crate::ExtensionTableDefinition::<u64, u64>::new(name);
        assert!(matches!(
            db.extension_write(|tx| {
                tx.open_table(&table)?;
                Ok(())
            })
            .await,
            Err(DbError::ReservedExtensionTable { .. })
        ));
    }
    Ok(())
}
