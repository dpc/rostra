//! Receipt-first, exact-current news mutation admission.

use rostra_core::EventId;
use rostra_core::event::{
    EventExt as _, SignedEvent, SocialPost, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::ToShort as _;

use crate::news_post_mutation::{
    NewsPostMutation, NewsPostMutationRequest, NewsPostMutationRequestId,
};
use crate::social_post_receipt::Receipt;
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

fn resolve(receipt: Receipt, digest: [u8; 32], db: &Database) -> DbResult<VerifiedEvent> {
    receipt.resolve(digest, db).map_err(|error| match error {
        DbError::SocialPostRequestMismatch => DbError::NewsPostMutationMismatch,
        other => other,
    })
}

impl Database {
    /// Prepare an owned, full-ID, canonical news target in one read snapshot.
    ///
    /// Known optional title, URL, body, legacy persona, declared tags and reply
    /// fields are supported; reactions and unknown fields are not. Original
    /// title-only news is eligible, but blank deleting payloads are not.
    /// This does not reserve the target. Errors use `SocialPostMutation*`.
    pub async fn prepare_news_post_mutation(
        &self,
        expected_current: EventId,
    ) -> DbResult<SocialPost> {
        self.read_with(|tx| {
            self.prepare_social_post_mutation_tx(
                expected_current,
                SocialPost::decode_mutable_news,
                &tx.open_table(&crate::events::TABLE)?,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &tx.open_table(&crate::social_posts_replaces::TABLE)?,
                &tx.open_table(&crate::social_posts_replaced_by::TABLE)?,
            )
        })
        .await
    }

    /// Return the original signed result without reading or reingesting
    /// payloads.
    pub async fn news_post_mutation_receipt(
        &self,
        id: NewsPostMutationRequestId,
        request: &NewsPostMutationRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::social_news_mutation_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| resolve(row.value_try()?, digest, self))
                .transpose()
        })
        .await
    }

    /// Atomically reconcile, compare the current target, admit and receipt.
    ///
    /// Trusted local publisher seam, not an authorization interface. Receipts
    /// win before validation, parsing, target lifecycle and admission. A new
    /// candidate must preserve all edit metadata or be the exact plain deletion
    /// payload. Failure rolls back the header, deletion and all projections.
    pub async fn commit_news_post_mutation(
        &self,
        id: NewsPostMutationRequestId,
        request: &NewsPostMutationRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_news_post_mutation_inner(
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

    pub(crate) async fn commit_news_post_mutation_inner(
        &self,
        id: NewsPostMutationRequestId,
        request: &NewsPostMutationRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::social_news_mutation_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return resolve(row.value_try()?, digest, self);
            }
            request.validate()?;
            let post = request.apply(self.prepare_social_post_mutation_tx(
                request.expected_current,
                SocialPost::decode_mutable_news,
                &tx.open_table(&crate::events::TABLE)?,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &tx.open_table(&crate::social_posts_replaces::TABLE)?,
                &tx.open_table(&crate::social_posts_replaced_by::TABLE)?,
            )?);
            let decode = match request.mutation {
                NewsPostMutation::Edit { .. } => SocialPost::decode_mutable_news,
                NewsPostMutation::Delete => SocialPost::decode_mutable_text,
            };
            if content.parent_aux() != Some(request.expected_current.to_short())
                || content
                    .content
                    .as_ref()
                    .and_then(|raw| decode(raw.as_ref()))
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
