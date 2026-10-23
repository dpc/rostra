use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;

use rostra_core::event::{
    Event, EventContentKind as _, EventExt as _, EventKind, SocialPost, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};

use crate::{Database, DbError, PayloadAdmissionConfig, PayloadAdmissionLimits};

fn post(
    secret: RostraIdSecretKey,
    parent: Option<ShortEventId>,
    time: u64,
) -> VerifiedEventContent {
    let raw = SocialPost::new_text(format!("candidate {time}"), None, Default::default())
        .serialize_cbor()
        .unwrap();
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent)
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

#[tokio::test(flavor = "multi_thread")]
async fn managed_head_serializes_empty_and_existing_head_consumers() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    let mut parent = None;
    for phase in 0..2 {
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = Vec::new();
        for n in 1..=8 {
            let db = db.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let content = post(secret, parent, phase * 10 + n);
                barrier.wait().await;
                db.commit_social_post_if_head(parent, &content, &mut None, &mut None)
                    .await
            }));
        }
        let mut winners = Vec::new();
        for task in tasks {
            match task.await? {
                Ok(event) => winners.push(event),
                Err(DbError::SocialPostHeadConflict) => {}
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(winners.len(), 1);
        assert_eq!(winners[0].parent_prev(), parent);
        parent = Some(winners[0].event_id.to_short());
        db.read_with(|tx| {
            assert_eq!(
                tx.open_table(&crate::events_self::TABLE)?
                    .range(..)?
                    .count(),
                phase as usize + 1
            );
            Ok(())
        })
        .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_head_admission_rollback_and_reopen_preserve_consumption() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("managed.redb");
    let db = Database::open(&path, secret.id()).await?;
    let root = post(secret, None, 1);
    db.commit_social_post_if_head(None, &root, &mut None, &mut None)
        .await?;
    let parent = Some(root.event_id().to_short());
    let candidate = post(secret, parent, 2);
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
    let mut allocation = db.reserve_payload_allocation(u64::from(candidate.content_len()))?;
    let mut buffer = None;
    assert!(matches!(
        db.commit_social_post_if_head(parent, &candidate, &mut allocation, &mut buffer)
            .await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    let candidate_id = candidate.event_id();
    drop(candidate);
    drop(buffer);
    drop(allocation);
    assert_eq!(db.get_self_current_head().await, parent);
    assert!(db.get_event(candidate_id).await.is_none());
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(db.payload_admission_usage().buffers, 0);
    drop(db);

    let db = Database::open(&path, secret.id()).await?;
    let candidate = post(secret, parent, 2);
    db.commit_social_post_if_head(parent, &candidate, &mut None, &mut None)
        .await?;
    drop(db); // Model loss of the caller's acknowledgment, not an OS kill.
    let db = Database::open(&path, secret.id()).await?;
    assert!(matches!(
        db.commit_social_post_if_head(parent, &post(secret, parent, 3), &mut None, &mut None)
            .await,
        Err(DbError::SocialPostHeadConflict)
    ));
    assert_eq!(
        db.get_self_current_head().await,
        Some(candidate.event_id().to_short())
    );
    Ok(())
}
