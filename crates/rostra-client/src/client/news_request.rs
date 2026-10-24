use std::time::Duration;

use rostra_client_db::DbError;
use rostra_client_db::news_post_request::{NewsPostRequest, NewsPostRequestId};
use rostra_client_db::vote_request::{VoteRequest, VoteRequestId};
use rostra_core::Timestamp;
use rostra_core::event::{
    Event, EventContentKind as _, EventKind, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::RostraIdSecretKey;
use snafu::ResultExt as _;
use tokio::time::{Instant, timeout_at};

use super::Client;
use super::follow_ordering::{FollowClock, WallClock};
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostError, PostResult, StorageSnafu};

impl Client {
    /// Publish one fixed-news/no-reply event per immutable persisted request.
    /// Permanent receipts reconcile before parsing input. Cancellation, storage
    /// errors and post-commit failures may be uncertain: retry identical input.
    /// Success means local acceptance, not remote delivery.
    pub async fn social_news_post_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: NewsPostRequestId,
        request: &NewsPostRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::NewsCandidate).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .news_post_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        let result = async {
            let post = request.to_social_post().context(StorageSnafu)?;
            let mut encoded = EncodedPayload::encode(&self.db, &post)?;
            let mut event = Event::builder_raw_content()
                .author(self.id)
                .kind(EventKind::SOCIAL_POST)
                .maybe_parent_prev(self.db.get_self_current_head().await)
                .content(&encoded.raw)
                .build();
            event.key_aux = id.event_aux_key(self.id);
            self.require_open_for_signing()?;
            let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
                .expect("Self-created event verifies");
            let mut acquired = AcquiredPayload {
                content: VerifiedEventContent::verify(event, encoded.raw)
                    .expect("Self-created content verifies"),
                buffer: None,
            };
            self.db
                .commit_news_post(
                    id,
                    request,
                    &acquired.content,
                    &mut encoded.capacity,
                    &mut acquired.buffer,
                )
                .await
                .context(StorageSnafu)
        }
        .await;
        if result.is_err()
            && let Some(event) = self
                .db
                .news_post_receipt(id, request)
                .await
                .context(StorageSnafu)?
        {
            return Ok(event);
        }
        result
    }

    /// Set an exact news target's singleton vote once per persisted request.
    /// An accepted old retry returns its original event without reapplying it.
    /// New up/down/clear require an available supported target. Local ordering
    /// waits at most two seconds for actual wall time strictly after the
    /// retained winner; later remote events can still supersede this
    /// accepted state.
    pub async fn set_social_vote_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: VoteRequestId,
        request: &VoteRequest,
    ) -> PostResult<VerifiedEvent> {
        self.set_social_vote_idempotent_clock(secret, id, request, &WallClock)
            .await
    }

    pub(super) async fn set_social_vote_idempotent_clock(
        &self,
        secret: RostraIdSecretKey,
        id: VoteRequestId,
        request: &VoteRequest,
        clock: &impl FollowClock,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::NewsCandidate).context(StorageSnafu);
        }
        let result = self.vote_request_loop(secret, id, request, clock).await;
        if result.is_err()
            && let Some(event) = self
                .db
                .vote_receipt(id, request)
                .await
                .context(StorageSnafu)?
        {
            return Ok(event);
        }
        result
    }

    async fn vote_request_loop(
        &self,
        secret: RostraIdSecretKey,
        id: VoteRequestId,
        request: &VoteRequest,
        clock: &impl FollowClock,
    ) -> PostResult<VerifiedEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.require_open_for_signing()?;
            if let Some(event) = self
                .db
                .vote_receipt(id, request)
                .await
                .context(StorageSnafu)?
            {
                return Ok(event);
            }
            let latest = self
                .db
                .vote_timestamp(request.target.1)
                .await
                .context(StorageSnafu)?;
            let now = clock.now();
            if let Some(latest) = latest {
                if latest == Timestamp::MAX {
                    return Err(PostError::VoteTimestampExhausted);
                }
                if now < latest {
                    return Err(PostError::VoteClockBehind { latest, now });
                }
            }
            if deadline <= Instant::now() {
                return Err(PostError::VoteOrderingBusy);
            }
            if latest == Some(now) {
                timeout_at(deadline, clock.wait_next_second())
                    .await
                    .map_err(|_| PostError::VoteOrderingBusy)?;
                continue;
            }
            let vote = request.to_social_vote();
            let mut encoded = EncodedPayload::encode(&self.db, &vote)?;
            let mut event = Event::builder_raw_content()
                .author(self.id)
                .kind(EventKind::SOCIAL_VOTE)
                .maybe_singleton_aux_key(vote.singleton_key_aux())
                .maybe_parent_prev(self.db.get_self_current_head().await)
                .content(&encoded.raw)
                .build();
            event.timestamp = now.into();
            self.require_open_for_signing()?;
            let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
                .expect("Self-created event verifies");
            let mut acquired = AcquiredPayload {
                content: VerifiedEventContent::verify(event, encoded.raw)
                    .expect("Self-created content verifies"),
                buffer: None,
            };
            match self
                .db
                .commit_vote(
                    id,
                    request,
                    &acquired.content,
                    &mut encoded.capacity,
                    &mut acquired.buffer,
                )
                .await
            {
                Err(DbError::VoteOrderingConflict) => continue,
                result => return result.context(StorageSnafu),
            }
        }
    }
}
