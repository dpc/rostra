use rostra_client_db::DbError;
use rostra_client_db::social_post_request::{SocialPostRequest, SocialPostRequestId};
use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Publish a text post once per caller request ID in this account database.
    ///
    /// Persist the random ID and immutable request before calling. Identical
    /// retries return the original signed result even after deletion/pruning;
    /// different input returns `DbError::SocialPostRequestMismatch` wrapped in
    /// `PostError::Storage`. Equality assumes BLAKE3 collision resistance.
    ///
    /// Receipts retain a digest and signed header indefinitely, not body bytes.
    /// They expose metadata and permit dictionary guesses; their fixed-size
    /// per-request storage is outside payload quotas and has no expiry.
    ///
    /// Cancellation, shutdown racing a started call, and lost responses do not
    /// promise rollback. Retry unchanged input after reopening the same durable
    /// DB. Closing clients reject new calls. Concurrent calls may sign
    /// discarded candidates, but only one candidate is committed. This is
    /// local acceptance, not remote delivery or deduplication across
    /// lost/restored/independent DBs.
    pub async fn social_post_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: SocialPostRequestId,
        request: &SocialPostRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::SocialPostCandidate).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .social_post_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        let mut encoded = EncodedPayload::encode(&self.db, &request.to_social_post())?;
        let parent = self.db.get_self_current_head().await;
        let aux = self.db.get_self_random_eventid().await;
        let event = Event::builder_raw_content()
            .author(self.id)
            .kind(EventKind::SOCIAL_POST)
            .maybe_parent_prev(parent)
            .maybe_parent_aux(aux)
            .content(&encoded.raw)
            .build();
        self.require_open_for_signing()?;
        let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
            .expect("Self-created event verifies");
        let content = VerifiedEventContent::verify(event, encoded.raw)
            .expect("Self-created content verifies");
        // Field drop order keeps bytes covered through rollback and hook panic.
        let mut acquired = AcquiredPayload {
            content,
            buffer: None,
        };
        self.db
            .commit_social_post_request(
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
