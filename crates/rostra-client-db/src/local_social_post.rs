use rostra_core::event::{EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::{ShortEventId, Timestamp};

use crate::{
    Database, DbError, DbResult, PayloadAllocation, PayloadBuffer, PayloadReservationOutcome,
    WriteTransactionCtx,
};

impl Database {
    /// Atomically check and consume a managed post's expected graph head.
    ///
    /// `None` requires an empty self-head set and a root candidate. Otherwise
    /// the candidate must use the supplied current head as its previous parent.
    /// Concurrent consumers serialize at this check, before any candidate
    /// header is committed. Admission refusal rolls back both check and
    /// ingestion.
    ///
    /// This trusted local-publication seam does not activate the client. The
    /// publisher retains charged candidate bytes through this call, dropping
    /// bytes before `buffer`, including on errors and post-commit panic.
    pub async fn commit_social_post_if_head(
        &self,
        expected: Option<ShortEventId>,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<VerifiedEvent> {
        self.write_with(|tx| {
            {
                let heads = tx.open_table(&crate::events_heads::TABLE)?;
                let matches = match expected {
                    Some(parent) => heads.get(&(self.self_id, parent))?.is_some(),
                    None => Self::read_head_tx(self.self_id, &heads)?.is_none(),
                };
                if !matches {
                    return Err(DbError::SocialPostHeadConflict);
                }
            }
            if content.parent_prev() != expected
                || (expected.is_none() && content.parent_aux().is_some())
            {
                return Err(DbError::SocialPostCandidate);
            }
            self.process_local_social_post_tx(tx, content, allocation, buffer)?;
            Ok(content.event)
        })
        .await
    }

    /// Preserve the one-transaction admission boundary for local post writers.
    pub(crate) fn process_local_social_post_tx(
        &self,
        tx: &WriteTransactionCtx,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<()> {
        self.process_local_social_post_candidate_tx(tx, content, allocation, buffer, false)
    }

    /// Share admission and rollback without relaxing ordinary create
    /// validation.
    pub(crate) fn process_local_social_post_candidate_tx(
        &self,
        tx: &WriteTransactionCtx,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        deletion: bool,
    ) -> DbResult<()> {
        if content.author() != self.self_id
            || content.kind() != EventKind::SOCIAL_POST
            || content.is_delete_parent_aux_content_set() != deletion
            || content.is_singleton()
            || content.content_len() >= Self::MAX_CONTENT_LEN
            || content.content.is_none()
        {
            return Err(DbError::SocialPostCandidate);
        }
        self.admit_local_candidate_tx(tx, content, allocation, buffer)
    }

    /// Admit an already validated local candidate and its payload atomically.
    pub(crate) fn admit_local_candidate_tx(
        &self,
        tx: &WriteTransactionCtx,
        content: &VerifiedEventContent,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<()> {
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
        self.process_event_content_with_buffer_tx(content, now, tx, buffer.as_ref())
    }
}
