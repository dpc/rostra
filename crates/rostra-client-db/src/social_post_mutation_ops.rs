//! Snapshot preparation and the authoritative receipt/compare/ingest
//! transaction.

use rostra_core::EventId;
use rostra_core::event::{
    EventExt as _, EventKind, SignedEvent, SocialPost, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::ToShort as _;

use crate::event::EventContentState;
use crate::social_post_mutation::{
    SocialPostMutation, SocialPostMutationRequest, SocialPostMutationRequestId,
};
use crate::social_post_receipt::Receipt;
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

fn resolve(receipt: Receipt, digest: [u8; 32], db: &Database) -> DbResult<VerifiedEvent> {
    receipt.resolve(digest, db).map_err(|err| match err {
        DbError::SocialPostRequestMismatch => DbError::SocialPostMutationMismatch,
        other => other,
    })
}

impl Database {
    /// Validate a full own current text target from one read-only snapshot.
    ///
    /// This does not reserve the target: publication repeats validation
    /// atomically. News, reactions, unknown fields, payloads over one MB
    /// and lineages over 1024 backward or forward steps are outside this
    /// narrow interface.
    pub async fn prepare_social_post_mutation(
        &self,
        expected_current: EventId,
    ) -> DbResult<SocialPost> {
        self.read_with(|tx| {
            self.prepare_social_post_mutation_tx(
                expected_current,
                &tx.open_table(&crate::events::TABLE)?,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &tx.open_table(&crate::social_posts_replaces::TABLE)?,
                &tx.open_table(&crate::social_posts_replaced_by::TABLE)?,
            )
        })
        .await
    }

    fn prepare_social_post_mutation_tx(
        &self,
        expected: EventId,
        events: &impl crate::events::ReadableTable,
        states: &impl crate::events_content_state::ReadableTable,
        store: &impl crate::content_store::ReadableTable,
        replaces: &impl crate::social_posts_replaces::ReadableTable,
        replaced_by: &impl crate::social_posts_replaced_by::ReadableTable,
    ) -> DbResult<SocialPost> {
        let short = expected.to_short();
        let event = Self::get_event_tx(short, events)?.ok_or(DbError::SocialPostMutationMissing)?;
        if event.event().compute_id() != expected {
            return Err(DbError::SocialPostMutationTargetMismatch);
        }
        if event.author() != self.self_id {
            return Err(DbError::SocialPostMutationNotOwner);
        }
        if event.kind() != EventKind::SOCIAL_POST || event.is_singleton() {
            return Err(DbError::SocialPostMutationUnsupported);
        }
        // Start at the retained root so a noncanonical sibling cannot mutate
        // itself merely because it has no descendants of its own.
        let mut current = short;
        let mut steps = 0;
        while let Some(row) = replaces
            .range(
                &(self.self_id, current, rostra_core::ShortEventId::ZERO)
                    ..=&(self.self_id, current, rostra_core::ShortEventId::MAX),
            )?
            .next()
            .transpose()?
        {
            steps += 1;
            if steps > 1024 {
                return Err(DbError::SocialPostMutationLineageLimit);
            }
            current = row.0.value().2;
        }
        let mut steps = 0;
        while let Some(row) = replaced_by
            .range(
                &(self.self_id, current, rostra_core::ShortEventId::ZERO)
                    ..=&(self.self_id, current, rostra_core::ShortEventId::MAX),
            )?
            .next()
            .transpose()?
        {
            steps += 1;
            if steps > 1024 {
                return Err(DbError::SocialPostMutationLineageLimit);
            }
            current = row.0.value().2;
        }
        if current != short {
            return Err(DbError::SocialPostMutationConflict);
        }
        match Self::get_event_content_state_tx(short, states)? {
            Some(EventContentState::Deleted { .. }) => {
                return Err(DbError::SocialPostMutationDeleted);
            }
            Some(EventContentState::Invalid) => return Err(DbError::SocialPostMutationUnsupported),
            Some(_) => return Err(DbError::SocialPostMutationUnavailable),
            None => {}
        }
        let raw = store
            .get(&event.content_hash())?
            .ok_or(DbError::SocialPostMutationUnavailable)?
            .value();
        let post = SocialPost::decode_mutable_text(raw.0.as_ref().as_ref())
            .ok_or(DbError::SocialPostMutationUnsupported)?;
        if event.is_delete_parent_aux_content_set()
            && post
                .djot_content
                .as_deref()
                .is_none_or(|body| body.trim().is_empty())
        {
            return Err(DbError::SocialPostMutationDeleted);
        }
        Ok(post)
    }

    /// Resolve an original committed result without reading its current
    /// payload.
    pub async fn social_post_mutation_receipt(
        &self,
        id: SocialPostMutationRequestId,
        request: &SocialPostMutationRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::social_post_mutation_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| resolve(row.value_try()?, digest, self))
                .transpose()
        })
        .await
    }

    /// Atomically reconcile or validate, ingest and record a local mutation.
    ///
    /// Trusted in-process publisher seam, not remote ingestion. Candidate bytes
    /// are independently rebound to the exact prepared payload before
    /// ingestion. Admission refusal rolls back deletion, header, projection
    /// and receipt. Callers retain charged bytes through completion,
    /// including hook panics.
    pub async fn commit_social_post_mutation(
        &self,
        id: SocialPostMutationRequestId,
        request: &SocialPostMutationRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_social_post_mutation_inner(
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

    pub(crate) async fn commit_social_post_mutation_inner(
        &self,
        id: SocialPostMutationRequestId,
        request: &SocialPostMutationRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::social_post_mutation_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return resolve(row.value_try()?, digest, self);
            }
            request.validate()?;
            let mut post = self.prepare_social_post_mutation_tx(
                request.expected_current,
                &tx.open_table(&crate::events::TABLE)?,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &tx.open_table(&crate::social_posts_replaces::TABLE)?,
                &tx.open_table(&crate::social_posts_replaced_by::TABLE)?,
            )?;
            post.djot_content = Some(match &request.mutation {
                SocialPostMutation::Edit { body } => body.clone(),
                SocialPostMutation::Delete => String::new(),
            });
            if content.parent_aux() != Some(request.expected_current.to_short())
                || content
                    .content
                    .as_ref()
                    .and_then(|raw| SocialPost::decode_mutable_text(raw.as_ref()))
                    .as_ref()
                    != Some(&post)
            {
                return Err(DbError::SocialPostCandidate);
            }
            self.process_local_social_post_candidate_tx(tx, content, allocation, buffer, true)?;
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
}
