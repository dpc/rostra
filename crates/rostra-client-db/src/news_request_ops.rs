//! Receipt-first admission and exact news target validation.

use rostra_core::event::{
    EventContentKind as _, EventExt as _, EventKind, SignedEvent, SocialPost, SocialVote,
    VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::ToShort as _;
use rostra_core::{EventId, Timestamp};

use crate::news_post_request::{NewsPostRequest, NewsPostRequestId};
use crate::social_post_receipt::Receipt;
use crate::vote_request::{VoteRequest, VoteRequestId};
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

fn resolve(
    receipt: Receipt,
    digest: [u8; 32],
    db: &Database,
    vote: bool,
) -> DbResult<VerifiedEvent> {
    receipt.resolve(digest, db).map_err(|error| match error {
        DbError::SocialPostRequestMismatch if vote => DbError::VoteRequestMismatch,
        DbError::SocialPostRequestMismatch => DbError::NewsRequestMismatch,
        other => other,
    })
}

impl Database {
    /// Return the original accepted news event before reparsing input or
    /// looking at mutable event state.
    pub async fn news_post_receipt(
        &self,
        id: NewsPostRequestId,
        request: &NewsPostRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::social_news_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| resolve(row.value_try()?, digest, self, false))
                .transpose()
        })
        .await
    }

    /// Atomically admit one independent news event and its permanent receipt.
    pub async fn commit_news_post(
        &self,
        id: NewsPostRequestId,
        request: &NewsPostRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_news_post_inner(
            id,
            request,
            content,
            allocation,
            buffer,
            #[cfg(test)]
            None,
        )
        .await
    }

    pub(crate) async fn commit_news_post_inner(
        &self,
        id: NewsPostRequestId,
        request: &NewsPostRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::social_news_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return resolve(row.value_try()?, digest, self, false);
            }
            let post = request.to_social_post()?;
            if content.aux_key() != id.event_aux_key(self.self_id)
                || content.parent_aux().is_some()
                || content
                    .content
                    .as_ref()
                    .and_then(|raw| SocialPost::decode_supported_news(raw.as_ref()))
                    .as_ref()
                    != Some(&post)
                || Self::get_event_tx(
                    content.event_id().to_short(),
                    &tx.open_table(&crate::events::TABLE)?,
                )?
                .is_some()
            {
                return Err(DbError::NewsCandidate);
            }
            self.process_local_social_post_tx(tx, content, allocation, buffer)?;
            receipts.insert(
                &id.0,
                &Receipt::V1 {
                    digest,
                    event: SignedEvent {
                        event: content.event.event,
                        sig: content.event.sig,
                    },
                },
            )?;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook(tx)?;
            }
            Ok(content.event)
        })
        .await
    }

    /// Reconcile the original vote without changing the current singleton.
    pub async fn vote_receipt(
        &self,
        id: VoteRequestId,
        request: &VoteRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::social_vote_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| resolve(row.value_try()?, digest, self, true))
                .transpose()
        })
        .await
    }

    /// Read the durable singleton clock without reserving its next value.
    /// The target author remains in the payload; the legacy singleton key uses
    /// only the shortened event ID.
    pub async fn vote_timestamp(&self, target: EventId) -> DbResult<Option<Timestamp>> {
        self.read_with(|tx| {
            Ok(tx
                .open_table(&crate::events_singletons_new::TABLE)?
                .get(&(
                    self.self_id,
                    EventKind::SOCIAL_VOTE,
                    rostra_core::event::EventAuxKey::from_bytes(target.to_short().to_bytes()),
                ))?
                .map(|row| row.value().ts))
        })
        .await
    }

    /// Atomically reconcile, validate, order, admit and receipt a singleton
    /// vote. A non-retry must strictly supersede the retained local winner
    /// by actual wall time, including winners learned from other devices.
    pub async fn commit_vote(
        &self,
        id: VoteRequestId,
        request: &VoteRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_vote_inner(
            id,
            request,
            content,
            allocation,
            buffer,
            #[cfg(test)]
            None,
        )
        .await
    }

    pub(crate) async fn commit_vote_inner(
        &self,
        id: VoteRequestId,
        request: &VoteRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::social_vote_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return resolve(row.value_try()?, digest, self, true);
            }
            let events = tx.open_table(&crate::events::TABLE)?;
            let event = Self::get_event_tx(request.target.1.to_short(), &events)?
                .ok_or(DbError::NewsTarget)?;
            if event.author() != request.target.0 || event.event().compute_id() != request.target.1
            {
                return Err(DbError::NewsTarget);
            }
            Self::supported_news_tx(
                &event,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &mut 0,
            )?;
            if tx
                .open_table(&crate::social_posts_by_time::TABLE)?
                .get(&(event.timestamp(), request.target.1.to_short()))?
                .is_none()
            {
                return Err(DbError::NewsUnavailable);
            }
            let vote = request.to_social_vote();
            if content.author() != self.self_id
                || content.kind() != EventKind::SOCIAL_VOTE
                || !content.is_singleton()
                || content.is_delete_parent_aux_content_set()
                || content.parent_aux().is_some()
                || Some(content.aux_key()) != vote.singleton_key_aux()
                || content.content_len() > 1_000_000
                || content
                    .content
                    .as_ref()
                    .and_then(|raw| raw.deserialize_cbor::<SocialVote>().ok())
                    .as_ref()
                    != Some(&vote)
            {
                return Err(DbError::NewsCandidate);
            }
            if content.timestamp() > Timestamp::now() {
                return Err(DbError::VoteFutureTimestamp);
            }
            let singletons = tx.open_table(&crate::events_singletons_new::TABLE)?;
            if singletons
                .get(&(self.self_id, EventKind::SOCIAL_VOTE, content.aux_key()))?
                .is_some_and(|row| content.timestamp() <= row.value().ts)
            {
                return Err(DbError::VoteOrderingConflict);
            }
            if Self::get_event_tx(content.event_id().to_short(), &events)?.is_some() {
                return Err(DbError::NewsCandidate);
            }
            drop(events);
            drop(singletons);
            self.admit_local_candidate_tx(tx, content, allocation, buffer)?;
            if tx
                .open_table(&crate::events_singletons_new::TABLE)?
                .get(&(self.self_id, EventKind::SOCIAL_VOTE, content.aux_key()))?
                .is_none_or(|row| row.value().inner.event_id != content.event_id().to_short())
            {
                return Err(DbError::NewsCandidate);
            }
            receipts.insert(
                &id.0,
                &Receipt::V1 {
                    digest,
                    event: SignedEvent {
                        event: content.event.event,
                        sig: content.event.sig,
                    },
                },
            )?;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook(tx)?;
            }
            Ok(content.event)
        })
        .await
    }

    pub(crate) fn supported_news_tx(
        event: &crate::EventRecord,
        states: &impl crate::events_content_state::ReadableTable,
        store: &impl crate::content_store::ReadableTable,
        bytes: &mut usize,
    ) -> DbResult<SocialPost> {
        if event.kind() != EventKind::SOCIAL_POST || event.is_singleton() {
            return Err(DbError::NewsTarget);
        }
        if Self::get_event_content_state_tx(event.event().compute_id().to_short(), states)?
            .is_some()
        {
            return Err(DbError::NewsUnavailable);
        }
        if event.content_len() > 1_000_000
            || bytes.saturating_add(event.content_len() as usize) > 16 * 1024 * 1024
        {
            return Err(DbError::NewsPayloadLimit);
        }
        let raw = store
            .get(&event.content_hash())?
            .ok_or(DbError::NewsUnavailable)?;
        let raw = raw.value();
        let raw = raw.0.as_ref().as_ref();
        *bytes = bytes.saturating_add(raw.len());
        if raw.len() > 1_000_000 || *bytes > 16 * 1024 * 1024 {
            return Err(DbError::NewsPayloadLimit);
        }
        SocialPost::decode_supported_news(raw).ok_or(DbError::NewsUnsupported)
    }
}
