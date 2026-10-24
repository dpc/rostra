//! Permanent non-replayable digest receipts, independent of payload lifecycle.

use bincode::{Decode, Encode};
use rostra_core::event::{
    EventExt as _, EventKind, SignedEvent, VerifiedEvent, VerifiedEventContent,
};

use crate::media_request::{MediaRequest, MediaRequestId};
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

/// Fixed-size permanent local journal row, outside payload retention quotas.
#[derive(Encode, Decode)]
pub(crate) enum MediaReceipt {
    /// V1 exact bytes/MIME/policy digest and original signed event.
    V1 {
        digest: [u8; 32],
        event: SignedEvent,
    },
}

impl MediaReceipt {
    fn resolve(&self, digest: [u8; 32], db: &Database) -> DbResult<VerifiedEvent> {
        let Self::V1 {
            digest: original,
            event,
        } = self;
        if *original != digest {
            return Err(DbError::MediaRequestMismatch);
        }
        if event.kind() != EventKind::SOCIAL_MEDIA {
            return Err(DbError::MediaCandidate);
        }
        VerifiedEvent::verify_signed(db.self_id, *event).map_err(|_| DbError::MediaCandidate)
    }
}

impl Database {
    /// Resolve an exact retry before current decoder, capacity or lifecycle
    /// checks.
    pub async fn media_receipt(
        &self,
        id: MediaRequestId,
        request: &MediaRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::media_request_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| row.value_try()?.resolve(digest, self))
                .transpose()
        })
        .await
    }

    /// Atomically admit trusted local publisher bytes and their permanent
    /// receipt.
    ///
    /// Caller must canonical-validate and encode exactly this request before
    /// signing. Allocation and buffer remain held through commit and rollback.
    /// A receipt race returns the original result without re-admitting bytes.
    pub async fn commit_media_request(
        &self,
        id: MediaRequestId,
        request: &MediaRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_media_request_inner(
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

    pub(crate) async fn commit_media_request_inner(
        &self,
        id: MediaRequestId,
        request: &MediaRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::media_request_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return row.value_try()?.resolve(digest, self);
            }
            if content.author() != self.self_id
                || content.kind() != EventKind::SOCIAL_MEDIA
                || content.is_singleton()
                || content.is_delete_parent_aux_content_set()
                || content.content_len() as usize > 2_000_128
                || content.content.is_none()
            {
                return Err(DbError::MediaCandidate);
            }
            self.admit_local_candidate_tx(tx, content, allocation, buffer)?;
            receipts.insert(
                &id.0,
                &MediaReceipt::V1 {
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
