use rostra_client_db::DbError;
use rostra_client_db::shoutbox_request::{ShoutboxRequest, ShoutboxRequestId};
use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Accept one shoutbox event per immutable persisted request.
    ///
    /// Success is local acceptance, not delivery. Cancellation, storage
    /// failures, or shutdown racing an executing transaction may be
    /// uncertain; retry the same key/input. New calls are refused after
    /// shutdown, including retries.
    pub async fn post_shoutbox_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: ShoutboxRequestId,
        request: &ShoutboxRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::ShoutboxCandidate).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .shoutbox_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        let result = async {
            let post = request.to_shoutbox().context(StorageSnafu)?;
            let mut encoded = EncodedPayload::encode(&self.db, &post)?;
            let mut event = Event::builder_raw_content()
                .author(self.id)
                .kind(EventKind::SHOUTBOX)
                .maybe_parent_prev(self.db.get_self_current_head().await)
                .content(&encoded.raw)
                .build();
            event.key_aux = id.event_aux_key(self.id);
            self.require_open_for_signing()?;
            let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
                .expect("Self-created event verifies");
            let mut acquired = AcquiredPayload {
                content: VerifiedEventContent::verify(event, encoded.raw)
                    .expect("Self-created content verifies"),
                buffer: None,
            };
            self.db
                .commit_shoutbox(
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
                .shoutbox_receipt(id, request)
                .await
                .context(StorageSnafu)?
        {
            return Ok(event);
        }
        result
    }
}
