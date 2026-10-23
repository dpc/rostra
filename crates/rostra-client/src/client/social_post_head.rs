use std::collections::BTreeSet;

use rostra_client_db::DbError;
use rostra_core::event::{
    Event, EventKind, PersonaTag, SocialPost, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::RostraIdSecretKey;
use rostra_core::{ExternalEventId, ShortEventId};
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Publish using a one-use current graph head, or an empty graph for
    /// `None`.
    ///
    /// Unlike ordinary `social_post`, the supplied head is both checked and
    /// consumed atomically. Sibling heads need not disappear. A stale head
    /// returns `DbError::SocialPostHeadConflict` through `PostError::Storage`.
    /// Existing text/reaction inference is preserved.
    ///
    /// This does not activate the client: managed hosts activate only after
    /// successful publication, so first-use initialization cannot invalidate an
    /// empty-graph request. Cancellation/activation failure after commit leaves
    /// the head consumed. This is not a durable request/result receipt; use
    /// `social_post_idempotent` for original-result reconciliation.
    pub async fn social_post_if_head(
        &self,
        secret: RostraIdSecretKey,
        expected: Option<ShortEventId>,
        body: String,
        reply_to: Option<ExternalEventId>,
        persona_tags: BTreeSet<PersonaTag>,
    ) -> PostResult<VerifiedEvent> {
        self.require_open_for_signing()?;
        if secret.id() != self.id {
            return Err(DbError::SocialPostCandidate).context(StorageSnafu);
        }
        let post = SocialPost::new(body, reply_to, persona_tags);
        let mut encoded = EncodedPayload::encode(&self.db, &post)?;
        let aux = if expected.is_some() {
            self.db.get_self_random_eventid().await
        } else {
            None
        };
        let event = Event::builder_raw_content()
            .author(self.id)
            .kind(EventKind::SOCIAL_POST)
            .maybe_parent_prev(expected)
            .maybe_parent_aux(aux)
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
            .commit_social_post_if_head(
                expected,
                &acquired.content,
                &mut encoded.capacity,
                &mut acquired.buffer,
            )
            .await
            .context(StorageSnafu)
    }
}
