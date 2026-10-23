use rostra_client_db::DbError;
use rostra_client_db::reaction_request::{ReactionRequest, ReactionRequestId};
use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Publish one independent Add or exact own-row Remove per persisted
    /// request. Change is unsupported; it never consumes a key. Receipts
    /// reconcile before target validation and survive deletion/pruning.
    /// This is local acceptance, not cross-device CAS or remote delivery.
    /// Cancellation or shutdown racing a started transaction may still
    /// commit; retry identical persisted input. Permanent digest/
    /// signed-header receipts have no individual expiry/erase.
    pub async fn react_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: ReactionRequestId,
        request: &ReactionRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::ReactionNotOwner).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .reaction_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        let post = match self.db.prepare_reaction(request).await {
            Ok(post) => post,
            Err(error) => {
                if let Some(event) = self
                    .db
                    .reaction_receipt(id, request)
                    .await
                    .context(StorageSnafu)?
                {
                    return Ok(event);
                }
                return Err(error).context(StorageSnafu);
            }
        };
        let mut encoded = EncodedPayload::encode(&self.db, &post)?;
        let parent = self.db.get_self_current_head().await;
        let mut event = Event::builder_raw_content()
            .author(self.id)
            .kind(EventKind::SOCIAL_POST)
            .maybe_parent_prev(parent)
            .maybe_delete(request.expected_current().map(|id| id.to_short()))
            .content(&encoded.raw)
            .build();
        event.key_aux = id.event_aux_key(self.id);
        self.require_open_for_signing()?;
        let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
            .expect("Self-created event verifies");
        let content = VerifiedEventContent::verify(event, encoded.raw)
            .expect("Self-created content verifies");
        let mut acquired = AcquiredPayload {
            content,
            buffer: None,
        };
        self.db
            .commit_reaction(
                id,
                request,
                &acquired.content,
                &mut encoded.capacity,
                &mut acquired.buffer,
            )
            .await
            .context(StorageSnafu)
    }
}
