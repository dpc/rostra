use rostra_client_db::DbError;
use rostra_client_db::media_request::{MediaRequest, MediaRequestId};
use rostra_core::event::content_kind::SocialMedia;
use rostra_core::event::{Event, EventAuxKey, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;
use rostra_core::static_media::validate_normalized_static_media;
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StaticMediaSnafu, StorageSnafu};

impl Client {
    /// Publish exact persisted normalized bytes once per random local request
    /// ID.
    ///
    /// Receipt lookup precedes canonical validation, mutable state and capacity
    /// checks. Exact retries after deletion/pruning return the original signed
    /// event without restoring bytes. Keep immutable pending bytes across lost
    /// responses and restart; never re-normalize them. Closing clients reject
    /// new calls; cancellation/shutdown of started calls does not imply
    /// rollback. Success is local acceptance, not delivery or
    /// cross-database deduplication.
    ///
    /// Fixed-size digest/header receipts never expire and are outside payload
    /// quotas; they permit guesses of low-entropy input. Decode runs in a
    /// blocking section; hosts must still bound concurrency and pending
    /// input lengths.
    pub async fn publish_media_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: MediaRequestId,
        request: &MediaRequest,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::MediaCandidate).context(StorageSnafu);
        }
        if let Some(event) = self
            .db
            .media_receipt(id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(event);
        }
        let result = async {
            tokio::task::block_in_place(|| {
                validate_normalized_static_media(
                    &request.mime,
                    &request.data,
                    request.policy_version,
                )
            })
            .context(StaticMediaSnafu)?;
            let media = SocialMedia {
                mime: request.mime.clone(),
                data: request.data.clone(),
            };
            let mut encoded = EncodedPayload::encode(&self.db, &media)?;
            let mut event = Event::builder_raw_content()
                .author(self.id)
                .kind(EventKind::SOCIAL_MEDIA)
                .maybe_parent_prev(self.db.get_self_current_head().await)
                .content(&encoded.raw)
                .build();
            // Non-singleton random nonce prevents identical independent uploads
            // collapsing into a single envelope when head/timestamp coincide.
            event.key_aux = EventAuxKey::from_bytes(rand::random());
            self.require_open_for_signing()?;
            let event = VerifiedEvent::verify_signed(self.id, event.signed_by(secret))
                .expect("self-created event verifies");
            let mut acquired = AcquiredPayload {
                content: VerifiedEventContent::verify(event, encoded.raw)
                    .expect("self-created content verifies"),
                buffer: None,
            };
            self.db
                .commit_media_request(
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
                .media_receipt(id, request)
                .await
                .context(StorageSnafu)?
        {
            return Ok(event);
        }
        result
    }
}
