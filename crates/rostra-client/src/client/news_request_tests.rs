use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rostra_client_db::news_post_request::{NewsPostRequest, NewsPostRequestId};
use rostra_client_db::vote_request::{VoteRequest, VoteRequestId};
use rostra_client_db::{Database, DbError};
use rostra_core::Timestamp;
use rostra_core::event::{
    Event, EventContentKind as _, EventExt as _, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::RostraIdSecretKey;

use super::Client;
use super::follow_ordering::FollowClock;
use crate::error::PostError;

struct Clock {
    /// Sampled actual test wall time.
    now: AtomicU64,
}

impl FollowClock for Clock {
    fn now(&self) -> Timestamp {
        self.now.load(Ordering::SeqCst).into()
    }
    async fn wait_next_second(&self) {
        self.now.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}

async fn client(
    db: Database,
    endpoint: &iroh::Endpoint,
    secret: RostraIdSecretKey,
) -> anyhow::Result<Arc<Client>> {
    Ok(Client::builder(secret.id())
        .db(db)
        .iroh_endpoint(endpoint.clone())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await?)
}

#[tokio::test(flavor = "multi_thread")]
async fn news_vote_public_retries_clocks_reopen_shutdown() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("client.redb");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let local = client(Database::open(&path, secret.id()).await?, &endpoint, secret).await?;
    let input = NewsPostRequest {
        body: "".into(),
        title: "Headline".into(),
        url: None,
    };
    let id = NewsPostRequestId([1; 16]);
    let (a, b) = tokio::join!(
        local.social_news_post_idempotent(secret, id, &input),
        local.social_news_post_idempotent(secret, id, &input)
    );
    let target = a?;
    assert_eq!(target.event_id, b?.event_id);
    let (a, b) = tokio::join!(
        local.social_news_post_idempotent(secret, NewsPostRequestId([2; 16]), &input),
        local.social_news_post_idempotent(secret, NewsPostRequestId([3; 16]), &input)
    );
    assert_ne!(a?.event_id, b?.event_id);
    let up = VoteRequest {
        target: (secret.id(), target.event_id),
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
    let clock = Clock {
        now: AtomicU64::new(10),
    };
    let a = local
        .set_social_vote_idempotent_clock(secret, VoteRequestId([1; 16]), &up, &clock)
        .await?;
    let b = local
        .set_social_vote_idempotent_clock(secret, VoteRequestId([2; 16]), &down, &clock)
        .await?;
    assert!(a.timestamp() < b.timestamp());
    assert_eq!(
        local
            .set_social_vote_idempotent(secret, VoteRequestId([1; 16]), &up)
            .await?
            .event_id,
        a.event_id
    );
    let latest = local.db.vote_timestamp(target.event_id).await?.unwrap();
    clock.now.store(latest.as_u64() - 1, Ordering::SeqCst);
    assert!(matches!(
        local
            .set_social_vote_idempotent_clock(secret, VoteRequestId([3; 16]), &clear, &clock)
            .await,
        Err(PostError::VoteClockBehind { .. })
    ));
    assert!(
        local
            .db
            .vote_receipt(VoteRequestId([3; 16]), &clear)
            .await?
            .is_none()
    );
    clock.now.store(latest.as_u64(), Ordering::SeqCst);
    let c = local
        .set_social_vote_idempotent_clock(secret, VoteRequestId([3; 16]), &clear, &clock)
        .await?;
    assert!(b.timestamp() < c.timestamp());
    let mut shutdown = local.clone().begin_shutdown().unwrap();
    assert!(matches!(
        local.social_news_post_idempotent(secret, id, &input).await,
        Err(PostError::ClientClosing)
    ));
    assert!(matches!(
        local
            .set_social_vote_idempotent(secret, VoteRequestId([1; 16]), &up)
            .await,
        Err(PostError::ClientClosing)
    ));
    drop(local);
    shutdown.join().await?;
    let local = client(Database::open(&path, secret.id()).await?, &endpoint, secret).await?;
    assert_eq!(
        local
            .social_news_post_idempotent(secret, id, &input)
            .await?
            .event_id,
        target.event_id
    );
    assert_eq!(
        local
            .set_social_vote_idempotent(secret, VoteRequestId([1; 16]), &up)
            .await?
            .event_id,
        a.event_id
    );
    let d = local
        .set_social_vote_idempotent_clock(secret, VoteRequestId([4; 16]), &up, &clock)
        .await?;
    assert!(c.timestamp() < d.timestamp());
    // A remote device may pin the existing singleton clock at MAX. The nonce
    // remains the target's singleton key and must never be replaced by request ID.
    let payload = down.to_social_vote();
    let raw = payload.serialize_cbor()?;
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .kind(rostra_core::event::EventKind::SOCIAL_VOTE)
        .maybe_singleton_aux_key(payload.singleton_key_aux())
        .content(&raw)
        .build();
    event.timestamp = Timestamp::MAX.into();
    let remote = VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))?,
        raw,
    )?;
    local.db.try_process_event_with_content(&remote).await?;
    assert!(matches!(
        local
            .set_social_vote_idempotent(secret, VoteRequestId([5; 16]), &up)
            .await,
        Err(PostError::VoteTimestampExhausted)
    ));
    assert_eq!(
        local
            .set_social_vote_idempotent(secret, VoteRequestId([1; 16]), &up)
            .await?
            .event_id,
        a.event_id
    );
    assert!(matches!(
        local
            .set_social_vote_idempotent(secret, VoteRequestId([1; 16]), &down)
            .await,
        Err(PostError::Storage {
            source: DbError::VoteRequestMismatch,
            ..
        })
    ));
    local.begin_shutdown().unwrap().join().await?;
    endpoint.close().await;
    Ok(())
}
