use rostra_client_db::{DbError, PayloadReservationOutcome};
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_dm::MessageBody;
use snafu::ResultExt as _;

use super::Client;
use crate::acquired_payload::AcquiredPayload;
use crate::encoded_payload::EncodedPayload;
use crate::error::{
    DirectMessageEncodingSnafu, DirectMessageUnavailableSnafu, PostResult, StorageSnafu,
};

impl Client {
    /// Start the owned lifecycle/decryption worker after full activation.
    pub(super) fn start_direct_message_worker(&self, secret: RostraIdSecretKey) {
        let handle = self.handle();
        let pending = self.db.dm_pending_notify();
        self.spawn_task(async move {
            let mut maintenance_due = tokio::time::Instant::now();
            loop {
                // Register before inspecting durable work; retain only the
                // notification, not a strong client, across the idle wait.
                let notified = pending.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let result = async {
                    let Some(client) = handle.app_ref_opt() else { return Ok(None) };
                    if maintenance_due <= tokio::time::Instant::now() {
                        match client.maintain_direct_messages(secret).await {
                            Ok(()) => {}
                            Err(crate::error::PostError::Storage {
                                source: DbError::PayloadAdmissionPaused { .. }
                            }) => {}
                            Err(error) => return Err(error),
                        }
                        maintenance_due = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                    }
                    let worked = client.db.dm_process_pending_now()
                        .await.context(StorageSnafu)?;
                    Ok(Some(worked))
                }.await;
                match result {
                    Ok(Some(worked)) => {
                        // Release the strong client reference and all key
                        // material before yielding, even under sustained load.
                        if worked {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        } else {
                            tokio::select! {
                                () = &mut notified => {}
                                () = tokio::time::sleep_until(maintenance_due) => {}
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(target: "rostra::direct_messages", %error, "Direct-message worker stopped");
                        break;
                    }
                }
            }
        });
    }

    /// Maintain this installation's live keys and announce a newly assigned
    /// epoch, without extending deadlines or republishing known metadata.
    pub async fn maintain_direct_messages(&self, secret: RostraIdSecretKey) -> PostResult<()> {
        self.require_dm_authority(secret)?;
        let announcement = self
            .db
            .dm_maintain_local_now()
            .await
            .context(StorageSnafu)?;
        let now = rostra_core::Timestamp::now().as_u64();
        if announcement
            .epoch
            .as_ref()
            .is_some_and(|epoch| now < epoch.send_from)
            || self
                .db
                .dm_announcement_known(&announcement)
                .await
                .context(StorageSnafu)?
        {
            return Ok(());
        }
        self.publish_dm_raw(secret, EventKind::DM_DEVICE, 74, None, || {
            announcement.encode()
        })
        .await?;
        Ok(())
    }

    /// Permanently stop selecting this account's device ID for future sends,
    /// once senders learn the retirement. This does not wipe retained history.
    pub async fn retire_direct_message_device(
        &self,
        secret: RostraIdSecretKey,
        device_id: [u8; 16],
    ) -> PostResult<VerifiedEvent> {
        self.require_dm_authority(secret)?;
        let announcement = rostra_dm::Announcement {
            device_id,
            epoch: None,
        };
        self.publish_dm_raw(secret, EventKind::DM_DEVICE, 18, None, || {
            announcement.encode()
        })
        .await
    }

    /// Re-enroll a retired local installation using a new random ID and key.
    pub async fn reenroll_direct_messages(&self, secret: RostraIdSecretKey) -> PostResult<()> {
        self.require_dm_authority(secret)?;
        self.db.dm_reenroll().await.context(StorageSnafu)?;
        self.maintain_direct_messages(secret).await
    }

    /// Encrypt and atomically retain a text message before publishing its head.
    ///
    /// The HTTP layer must independently enforce unlocked full-session access;
    /// another session's active client is not authority to read or send
    /// messages.
    pub async fn send_direct_message(
        &self,
        secret: RostraIdSecretKey,
        recipient: RostraId,
        text: String,
    ) -> PostResult<VerifiedEvent> {
        self.require_dm_authority(secret)?;
        let destinations = self
            .db
            .dm_destinations_now(recipient)
            .await
            .context(StorageSnafu)?;
        let body =
            MessageBody::new(self.id, recipient, text).context(DirectMessageEncodingSnafu)?;
        let size = rostra_dm::FRAME_OVERHEAD
            + rostra_dm::text_bucket(body.text().len()).context(DirectMessageEncodingSnafu)?;
        self.publish_dm_raw(
            secret,
            EventKind::DIRECT_MESSAGE,
            size,
            Some((&body, &destinations)),
            || rostra_dm::encrypt(&body, destinations.keys()),
        )
        .await
    }

    /// Check full active-client authority using the calling session's secret.
    ///
    /// A shared active client alone must never authorize plaintext access.
    pub fn require_dm_authority(&self, secret: RostraIdSecretKey) -> PostResult<()> {
        self.require_open_for_signing()?;
        if !self.is_mode_full
            || !self.active.load(std::sync::atomic::Ordering::SeqCst)
            || secret.id() != self.id
        {
            return DirectMessageUnavailableSnafu.fail();
        }
        Ok(())
    }

    async fn publish_dm_raw(
        &self,
        secret: RostraIdSecretKey,
        kind: EventKind,
        size: usize,
        body: Option<(&MessageBody, &rostra_client_db::dm::SendPermit)>,
        encode: impl FnOnce() -> Result<Vec<u8>, rostra_dm::Error>,
    ) -> PostResult<VerifiedEvent> {
        // The envelope encoder preallocates one age file and one outer frame;
        // conversion to Arc may temporarily coexist with the final Vec. Codec
        // internals and the caller-owned text are outside the payload ledger.
        let capacity = self
            .db
            .reserve_payload_allocation((2 * size) as u64)
            .map_err(|reason| DbError::PayloadAdmissionPaused { reason })
            .context(StorageSnafu)?;
        let mut encoded = EncodedPayload {
            raw: EventContentRaw::new(encode().context(DirectMessageEncodingSnafu)?),
            capacity,
        };
        let event = Event::builder_raw_content()
            .author(self.id)
            .kind(kind)
            .maybe_parent_prev(self.db.get_self_current_head().await)
            .maybe_parent_aux(self.db.get_self_random_eventid().await)
            .content(&encoded.raw)
            .build()
            .signed_by(secret);
        let verified = VerifiedEvent::verify_signed(self.id, event)
            .expect("self-created DM event must verify");
        let content = VerifiedEventContent::verify(verified, encoded.raw)
            .expect("self-created DM content must verify");
        let buffer = if let Some(allocation) = encoded.capacity.as_mut() {
            match self
                .db
                .reserve_payload(&verified)
                .await
                .context(StorageSnafu)?
            {
                PayloadReservationOutcome::Reserved(reservation) => Some(
                    allocation
                        .bind_to_reservation(&reservation)
                        .map_err(|reason| DbError::PayloadAdmissionPaused { reason })
                        .context(StorageSnafu)?,
                ),
                PayloadReservationOutcome::Deferred(reason) => {
                    return Err(DbError::PayloadAdmissionPaused { reason }).context(StorageSnafu);
                }
                _ => None,
            }
        } else {
            None
        };
        let acquired = AcquiredPayload { content, buffer };
        if let Some((body, permit)) = body {
            acquired
                .ingest_dm(&self.db, body, permit)
                .await
                .context(StorageSnafu)?;
        } else {
            acquired.ingest(&self.db).await.context(StorageSnafu)?;
        }
        Ok(verified)
    }
}
