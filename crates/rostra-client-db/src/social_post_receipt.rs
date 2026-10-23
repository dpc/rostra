//! Non-replayable local publication journal, deliberately independent of
//! payload deletion and retention. Fixed-size rows never expire.

use bincode::{Decode, Encode};
use rostra_core::Timestamp;
use rostra_core::event::{
    EventExt as _, EventKind, SignedEvent, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::ToShort as _;

use crate::social_post_request::{SocialPostRequest, SocialPostRequestId};
use crate::{
    Database, DbError, DbResult, PayloadAllocation, PayloadBuffer, PayloadReservationOutcome,
};

/// Versioned authoritative input digest and original signed result.
#[derive(Encode, Decode)]
pub(crate) enum Receipt {
    /// BLAKE3 canonical V1 input identity, not a reversible input journal.
    V1 {
        digest: [u8; 32],
        event: SignedEvent,
    },
}

impl Receipt {
    fn resolve(&self, digest: [u8; 32], db: &Database) -> DbResult<VerifiedEvent> {
        let Self::V1 {
            digest: original,
            event,
        } = self;
        if *original != digest {
            return Err(DbError::SocialPostRequestMismatch);
        }
        VerifiedEvent::verify_signed(db.self_id, *event).map_err(|_| DbError::SocialPostCandidate)
    }
}

impl Database {
    /// Look up a committed request without consulting current post/target
    /// state.
    ///
    /// Digest equality assumes BLAKE3 collision/second-preimage resistance.
    pub async fn social_post_receipt(
        &self,
        id: SocialPostRequestId,
        request: &SocialPostRequest,
    ) -> DbResult<Option<VerifiedEvent>> {
        let digest = request.digest(self.self_id);
        self.read_with(|tx| {
            tx.open_table(&crate::social_post_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| row.value_try()?.resolve(digest, self))
                .transpose()
        })
        .await
    }

    /// Atomically ingest a trusted local publisher's candidate and its receipt.
    ///
    /// The caller must encode exactly `request.to_social_post()` into `content`
    /// and retain its allocation through this call. Unlike `reserve_payload`,
    /// this never commits a candidate header before the receipt. Capacity
    /// errors roll back both. A receipt race discards the losing candidate.
    ///
    /// `buffer` remains owned by the caller until candidate bytes are dropped,
    /// including after a storage error or post-commit panic.
    pub async fn commit_social_post_request(
        &self,
        id: SocialPostRequestId,
        request: &SocialPostRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.commit_social_post_request_inner(
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

    pub(crate) async fn commit_social_post_request_inner(
        &self,
        id: SocialPostRequestId,
        request: &SocialPostRequest,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<ReceiptTestHook>,
    ) -> DbResult<VerifiedEvent> {
        let digest = request.digest(self.self_id);
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::social_post_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return row.value_try()?.resolve(digest, self);
            }
            if content.author() != self.self_id
                || content.kind() != EventKind::SOCIAL_POST
                || content.is_delete_parent_aux_content_set()
                || content.is_singleton()
                || content.content_len() >= Self::MAX_CONTENT_LEN
                || content.content.is_none()
            {
                return Err(DbError::SocialPostCandidate);
            }
            if let Some((author, id)) = request.reply_to {
                let event =
                    Self::get_event_tx(id.to_short(), &tx.open_table(&crate::events::TABLE)?)?
                        .ok_or(DbError::SocialPostReplyTarget)?;
                if event.author() != author
                    || event.kind() != EventKind::SOCIAL_POST
                    || event.event().compute_id() != id
                {
                    return Err(DbError::SocialPostReplyTarget);
                }
            }
            let now = Timestamp::now();
            self.process_event_tx(&content.event, now, tx)?;
            match self.reserve_payload_tx(tx, &content.event)? {
                PayloadReservationOutcome::Reserved(reservation) => {
                    *buffer = Some(
                        allocation
                            .as_mut()
                            .ok_or(DbError::SocialPostCandidate)?
                            .bind_to_reservation(&reservation)
                            .map_err(|reason| DbError::PayloadAdmissionPaused { reason })?,
                    );
                }
                PayloadReservationOutcome::Deferred(reason) => {
                    return Err(DbError::PayloadAdmissionPaused { reason });
                }
                _ => {}
            }
            self.process_event_content_with_buffer_tx(content, now, tx, buffer.as_ref())?;
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

#[cfg(test)]
pub(crate) type ReceiptTestHook =
    Box<dyn FnOnce(&crate::WriteTransactionCtx) -> DbResult<()> + Send>;
