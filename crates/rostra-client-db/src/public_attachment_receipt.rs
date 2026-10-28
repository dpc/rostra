//! Permanent digest-only journal, independent of public payload lifecycle.

use bincode::{Decode, Encode};
use rostra_core::event::{
    EventContentKind as _, EventExt as _, EventKind, SignedEvent, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::ToShort as _;

use crate::public_attachment_content::PublicAttachmentContent;
use crate::public_attachment_request::{PublicAttachmentRequest, PublicAttachmentRequestId};
use crate::{Database, DbError, DbResult, PayloadAllocation, PayloadBuffer};

/// Fixed-size authoritative row, outside payload quotas and never replayed.
#[derive(Encode, Decode)]
pub(crate) enum PublicAttachmentReceipt {
    /// V1 account/constant-MIME/exact-byte digest and original public envelope.
    V1 {
        /// Domain-separated digest, not recoverable file storage.
        digest: [u8; 32],
        /// Original signed event; no payload or signing secret.
        event: SignedEvent,
    },
}

impl PublicAttachmentReceipt {
    fn resolve(
        &self,
        digest: [u8; 32],
        id: PublicAttachmentRequestId,
        db: &Database,
    ) -> DbResult<VerifiedEvent> {
        let Self::V1 {
            digest: original,
            event,
        } = self;
        if *original != digest {
            return Err(DbError::PublicAttachmentRequestMismatch);
        }
        if event.kind() != EventKind::SOCIAL_MEDIA
            || event.is_singleton()
            || event.is_delete_parent_aux_content_set()
            || event.aux_key() != id.event_aux_key(db.self_id)
        {
            return Err(DbError::PublicAttachmentCandidate);
        }
        VerifiedEvent::verify_signed(db.self_id, *event)
            .map_err(|_| DbError::PublicAttachmentCandidate)
    }
}

impl Database {
    /// Resolve exact immutable input before current size/admission/lifecycle
    /// checks.
    ///
    /// This is a trusted integration primitive, not caller authorization.
    pub async fn public_attachment_receipt(
        &self,
        id: PublicAttachmentRequestId,
        request: &PublicAttachmentRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::media_public_attachment_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| row.value_try()?.resolve(digest, id, self))
                .transpose()
        })
        .await
    }

    /// Atomically admit exact public bytes, their event and a permanent
    /// receipt.
    ///
    /// Trusted local publisher primitive, not an authorization interface. A
    /// receipt race returns the original result without re-admitting bytes.
    /// Allocation and buffer remain held through commit or rollback.
    pub async fn commit_public_attachment_request(
        &self,
        id: PublicAttachmentRequestId,
        request: &PublicAttachmentRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_public_attachment_request_inner(
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

    pub(crate) async fn commit_public_attachment_request_inner(
        &self,
        id: PublicAttachmentRequestId,
        request: &PublicAttachmentRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::media_public_attachment_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return row.value_try()?.resolve(digest, id, self);
            }
            request.validate()?;
            if content.author() != self.self_id
                || content.kind() != EventKind::SOCIAL_MEDIA
                || content.is_singleton()
                || content.is_delete_parent_aux_content_set()
                || content.parent_aux().is_some()
                || content.aux_key() != id.event_aux_key(self.self_id)
                || content.content_len() as usize > 2_000_128
                || content.content.is_none()
            {
                return Err(DbError::PublicAttachmentCandidate);
            }
            // Compare the canonical wire hash without allocating another body.
            let mut hash = blake3::Hasher::new();
            PublicAttachmentContent::new(request)
                .serialize_cbor_to_writer(&mut hash)
                .map_err(|_| DbError::PublicAttachmentCandidate)?;
            if content.content_hash() != hash.finalize().into()
                || Self::get_event_tx(
                    content.event_id().to_short(),
                    &tx.open_table(&crate::events::TABLE)?,
                )?
                .is_some()
            {
                return Err(DbError::PublicAttachmentCandidate);
            }
            self.admit_local_candidate_tx(tx, content, allocation, buffer)?;
            receipts.insert(
                &id.0,
                &PublicAttachmentReceipt::V1 {
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
