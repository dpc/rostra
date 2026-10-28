use rostra_client_db::DbError;
use rostra_client_db::public_attachment_content::PublicAttachmentContent;
use rostra_client_db::public_attachment_request::{
    PublicAttachmentRequest, PublicAttachmentRequestId,
};
use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Publish immutable original public bytes once per persisted random
    /// request ID.
    ///
    /// Accepts 1..=1,000,000 bytes with fixed `application/octet-stream`,
    /// without decoding, normalization, filenames or MIME inference. This
    /// is not the normalized image API and does not make bytes safe to open
    /// or execute.
    ///
    /// Persist the exact bytes and ID before calling. Open-client and matching
    /// signing-owner checks precede receipt lookup; hosts enforce their own
    /// requesting session boundary. No active-client or DM authorization is
    /// added. Exact retries return the original envelope before mutable
    /// admission or lifecycle checks, never restoring pruned/deleted bytes.
    ///
    /// Permanent account-scoped receipts retain only a digest and signed public
    /// envelope, outside payload quotas. Digests permit input dictionary
    /// guesses. Success means local acceptance, not delivery or
    /// cross-database deduplication. Cancellation/lost acknowledgment does
    /// not imply rollback. Hosts must bound pending sizes/concurrency and
    /// keep whole-buffer work off the UI executor.
    pub async fn publish_public_attachment_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: PublicAttachmentRequestId,
        request: &PublicAttachmentRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::PublicAttachmentCandidate).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .public_attachment_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        let result = async {
            request.validate().context(StorageSnafu)?;
            let mut encoded =
                EncodedPayload::encode(&self.db, &PublicAttachmentContent::new(request))?;
            let mut event = Event::builder_raw_content()
                .author(self.id)
                .kind(EventKind::SOCIAL_MEDIA)
                .maybe_parent_prev(self.db.get_self_current_head().await)
                .content(&encoded.raw)
                .build();
            event.key_aux = id.event_aux_key(self.id);
            self.require_open_for_signing()?;
            let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
                .expect("self-created event verifies");
            let mut acquired = AcquiredPayload {
                content: VerifiedEventContent::verify(event, encoded.raw)
                    .expect("self-created content verifies"),
                buffer: None,
            };
            self.db
                .commit_public_attachment_request(
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
                .public_attachment_receipt(id, request)
                .await
                .context(StorageSnafu)?
        {
            return Ok(event);
        }
        result
    }
}
