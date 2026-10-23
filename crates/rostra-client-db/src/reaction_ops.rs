//! Receipt-first atomic independent additions and exact selected-row removals.

use std::collections::BTreeSet;

use rostra_core::ExternalEventId;
use rostra_core::event::{
    EventExt as _, SignedEvent, SocialPost, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::ToShort as _;

use crate::reaction_read::{self, Budget};
use crate::reaction_request::{ReactionOperation, ReactionRequest, ReactionRequestId};
use crate::social_post_receipt::Receipt;
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

fn resolve(receipt: Receipt, digest: [u8; 32], db: &Database) -> DbResult<VerifiedEvent> {
    receipt.resolve(digest, db).map_err(|error| match error {
        DbError::SocialPostRequestMismatch => DbError::ReactionMismatch,
        other => other,
    })
}

impl Database {
    /// Retrieve the original accepted event without target or payload
    /// validation.
    pub async fn reaction_receipt(
        &self,
        id: ReactionRequestId,
        request: &ReactionRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::social_reaction_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| resolve(row.value_try()?, digest, self))
                .transpose()
        })
        .await
    }

    /// Prepare a candidate payload from a snapshot; this reserves no target.
    pub async fn prepare_reaction(&self, request: &ReactionRequest) -> DbResult<SocialPost> {
        self.read_with(|tx| {
            self.prepare_reaction_tx(
                request,
                &tx.open_table(&crate::events::TABLE)?,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &tx.open_table(&crate::social_posts_replaces::TABLE)?,
                &tx.open_table(&crate::social_posts_replaced_by::TABLE)?,
            )
        })
        .await
    }

    fn prepare_reaction_tx(
        &self,
        request: &ReactionRequest,
        events: &impl crate::events::ReadableTable,
        states: &impl crate::events_content_state::ReadableTable,
        store: &impl crate::content_store::ReadableTable,
        replaces: &impl crate::social_posts_replaces::ReadableTable,
        replaced_by: &impl crate::social_posts_replaced_by::ReadableTable,
    ) -> DbResult<SocialPost> {
        request.validate()?;
        let target = reaction_read::bound_event(request.target, events)?;
        let mut budget = Budget::default();
        let versions = reaction_read::versions(
            request.target.0,
            request.target.1.to_short(),
            events,
            replaces,
            replaced_by,
            &mut budget,
        )?;
        match &request.operation {
            ReactionOperation::Add { reaction } => {
                if versions.last() != Some(&request.target.1.to_short()) {
                    return Err(DbError::ReactionConflict);
                }
                reaction_read::payload(&target, states, store, &mut budget, false)?;
                SocialPost::new_reaction(
                    reaction.clone(),
                    ExternalEventId::new(request.target.0, request.target.1),
                )
                .ok_or(DbError::ReactionInvalidInput)
            }
            ReactionOperation::Remove { expected_current } => {
                let row = Self::get_event_tx(expected_current.to_short(), events)?
                    .ok_or(DbError::ReactionTarget)?;
                if row.event().compute_id() != *expected_current {
                    return Err(DbError::ReactionTarget);
                }
                if row.author() != self.self_id {
                    return Err(DbError::ReactionNotOwner);
                }
                reaction_read::bound_event((self.self_id, *expected_current), events)?;
                let row_versions = reaction_read::versions(
                    self.self_id,
                    expected_current.to_short(),
                    events,
                    replaces,
                    replaced_by,
                    &mut budget,
                )?;
                if row_versions.last() != Some(&expected_current.to_short()) {
                    return Err(DbError::ReactionConflict);
                }
                let post = reaction_read::payload(&row, states, store, &mut budget, true)?;
                let reply = post.reply_to.ok_or(DbError::ReactionUnsupported)?;
                if reply.rostra_id() != request.target.0 || !versions.contains(&reply.event_id()) {
                    return Err(DbError::ReactionTarget);
                }
                // Deletion has no projection. Do not copy incidental metadata into
                // it or reinterpret e-only deleting payloads as replacements.
                Ok(SocialPost::new_text(String::new(), None, BTreeSet::new()))
            }
            ReactionOperation::Change { .. } => Err(DbError::ReactionUnsupported),
        }
    }

    /// Atomically reconcile or validate, admit, ingest and receipt a reaction.
    /// Candidate bytes and exact deletion parent are independently rebound
    /// here. Admission errors roll back the header, deletion, projection
    /// and receipt.
    pub async fn commit_reaction(
        &self,
        id: ReactionRequestId,
        request: &ReactionRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::social_reaction_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return resolve(row.value_try()?, digest, self);
            }
            let post = self.prepare_reaction_tx(
                request,
                &tx.open_table(&crate::events::TABLE)?,
                &tx.open_table(&crate::events_content_state::TABLE)?,
                &tx.open_table(&crate::content_store::TABLE)?,
                &tx.open_table(&crate::social_posts_replaces::TABLE)?,
                &tx.open_table(&crate::social_posts_replaced_by::TABLE)?,
            )?;
            let decoded = content.content.as_ref().and_then(|raw| {
                if matches!(request.operation, ReactionOperation::Add { .. }) {
                    SocialPost::decode_mutable_reaction(raw.as_ref())
                } else {
                    SocialPost::decode_mutable_text(raw.as_ref())
                }
            });
            if content.parent_aux() != request.expected_current().map(|id| id.to_short())
                || content.is_delete_parent_aux_content_set()
                    != request.expected_current().is_some()
                || content.aux_key() != id.event_aux_key(self.self_id)
                || decoded.as_ref() != Some(&post)
            {
                return Err(DbError::SocialPostCandidate);
            }
            if Self::get_event_tx(
                content.event_id().to_short(),
                &tx.open_table(&crate::events::TABLE)?,
            )?
            .is_some()
            {
                return Err(DbError::ReactionConflict);
            }
            self.process_local_social_post_candidate_tx(
                tx,
                content,
                allocation,
                buffer,
                request.expected_current().is_some(),
            )?;
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
            Ok(content.event)
        })
        .await
    }
}
