use rostra_client_db::DbError;
use rostra_client_db::news_post_mutation::{NewsPostMutationRequest, NewsPostMutationRequestId};
use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Correct an owned news body's exact bytes or explicitly delete its
    /// version.
    ///
    /// Persist this API's discriminator, random key and exact input before
    /// calling; legacy text mutation keys must never be rerouted here on retry.
    /// Receipts precede current target checks and never resurrect payloads.
    /// Edits preserve known news/persona/reply metadata, including title-only
    /// originals, but require a nonblank new body. Delete emits plain blank
    /// text.
    ///
    /// Local canonical compare-and-set is atomic with admission and permanent
    /// receipt insertion, not a cross-device CAS or delivery promise. Later
    /// remote events can supersede the result. Receipt rows are outside payload
    /// quotas and their digests permit offline input guesses. Cancellation or a
    /// lost acknowledgment can follow commit: retry identical input. Closing
    /// clients reject new calls, including receipt-only retries.
    pub async fn mutate_news_post_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: NewsPostMutationRequestId,
        request: &NewsPostMutationRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::SocialPostMutationNotOwner).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .news_post_mutation_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        request.validate().context(StorageSnafu)?;
        let post = match self
            .db
            .prepare_news_post_mutation(request.expected_current)
            .await
        {
            Ok(post) => post,
            Err(error) => {
                if let Some(event) = self
                    .db
                    .news_post_mutation_receipt(id, request)
                    .await
                    .context(StorageSnafu)?
                {
                    return Ok(event);
                }
                return Err(error).context(StorageSnafu);
            }
        };
        let mut encoded = EncodedPayload::encode(&self.db, &request.apply(post))?;
        if encoded.raw.as_ref().len() > 1_000_000 {
            return Err(DbError::SocialPostMutationTooLarge).context(StorageSnafu);
        }
        let parent = self.db.get_self_current_head().await;
        let event = Event::builder_raw_content()
            .author(self.id)
            .kind(EventKind::SOCIAL_POST)
            .maybe_parent_prev(parent)
            .maybe_delete(Some(request.expected_current.to_short()))
            .content(&encoded.raw)
            .build();
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
            .commit_news_post_mutation(
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
