use rostra_client_db::DbError;
use rostra_client_db::social_post_mutation::{
    SocialPostMutation, SocialPostMutationRequest, SocialPostMutationRequestId,
};
use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Mutate one owned text/reply version once per persisted immutable
    /// request.
    ///
    /// Original receipts reconcile before target validation, including after
    /// deletion/pruning. News, reactions and unknown fields are unsupported;
    /// supported reply/persona metadata is preserved. Blank edits are rejected.
    /// The full expected current version is checked atomically with ingestion.
    /// This is local compare-and-set, not a promise against later remote
    /// events.
    ///
    /// IDs have a separate mutation namespace in this account DB. Permanent
    /// receipts retain only an input digest and signed original event, outside
    /// payload quotas. Digests permit offline guesses, not plaintext recovery.
    /// Cancellation or shutdown racing a started call may still commit: retry
    /// identical input. New calls after closing fail, even receipt-only
    /// retries.
    pub async fn mutate_social_post_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: SocialPostMutationRequestId,
        request: &SocialPostMutationRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::SocialPostMutationNotOwner).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .social_post_mutation_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        request.validate().context(StorageSnafu)?;
        let mut post = match self
            .db
            .prepare_social_post_mutation(request.expected_current)
            .await
        {
            Ok(post) => post,
            Err(error) => {
                // A duplicate caller may have committed between the receipt
                // read and target preparation. Never misreport its deletion.
                if let Some(event) = self
                    .db
                    .social_post_mutation_receipt(id, request)
                    .await
                    .context(StorageSnafu)?
                {
                    return Ok(event);
                }
                return Err(error).context(StorageSnafu);
            }
        };
        post.djot_content = Some(match &request.mutation {
            SocialPostMutation::Edit { body } => body.clone(),
            SocialPostMutation::Delete => String::new(),
        });
        let mut encoded = EncodedPayload::encode(&self.db, &post)?;
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
            .commit_social_post_mutation(
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
