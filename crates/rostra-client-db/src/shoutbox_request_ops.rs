//! Receipt-first, atomic local shoutbox acceptance.

use rostra_core::event::content_kind::Shoutbox;
use rostra_core::event::{
    EventExt as _, EventKind, SignedEvent, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::ToShort as _;

use crate::shoutbox_request::{ShoutboxRequest, ShoutboxRequestId};
use crate::social_post_receipt::Receipt;
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

fn resolve(receipt: Receipt, digest: [u8; 32], db: &Database) -> DbResult<VerifiedEvent> {
    receipt.resolve(digest, db).map_err(|error| match error {
        DbError::SocialPostRequestMismatch => DbError::ShoutboxRequestMismatch,
        other => other,
    })
}

impl Database {
    /// Reconcile before validation or mutable-state checks, without
    /// reingestion.
    pub async fn shoutbox_receipt(
        &self,
        id: ShoutboxRequestId,
        request: &ShoutboxRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::shoutbox_request_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| resolve(row.value_try()?, digest, self))
                .transpose()
        })
        .await
    }

    /// Atomically admit a validated candidate and its permanent result receipt.
    pub async fn commit_shoutbox(
        &self,
        id: ShoutboxRequestId,
        request: &ShoutboxRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_shoutbox_inner(
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

    pub(crate) async fn commit_shoutbox_inner(
        &self,
        id: ShoutboxRequestId,
        request: &ShoutboxRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::shoutbox_request_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return resolve(row.value_try()?, digest, self);
            }
            let post = request.to_shoutbox()?;
            if content.author() != self.self_id
                || content.kind() != EventKind::SHOUTBOX
                || content.is_singleton()
                || content.is_delete_parent_aux_content_set()
                || content.parent_aux().is_some()
                || content.aux_key() != id.event_aux_key(self.self_id)
                || content
                    .content
                    .as_ref()
                    .and_then(|raw| Shoutbox::decode_supported(raw.as_ref()))
                    .as_ref()
                    != Some(&post)
                || Self::get_event_tx(
                    content.event_id().to_short(),
                    &tx.open_table(&crate::events::TABLE)?,
                )?
                .is_some()
            {
                return Err(DbError::ShoutboxCandidate);
            }
            self.admit_local_candidate_tx(tx, content, allocation, buffer)?;
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
