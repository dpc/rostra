//! Atomic installation, epoch, announcement and permanent public-intent
//! journal.

use bincode::{Decode, Encode};
use rostra_core::Timestamp;
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, SignedEvent, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::RostraIdSecretKey;
use rostra_dm::{Announcement, LocalEpoch};
use snafu::ResultExt as _;

use crate::dm::Installation;
use crate::dm_device_request::{
    DmDeviceOutcome, DmDeviceRequest, DmDeviceRequestId, DmEnrollmentExpectation,
};
use crate::{Database, DbError, DbResult, DirectMessageSnafu, PayloadAllocation, PayloadBuffer};

/// Permanent public metadata only; never retains an epoch secret or plaintext.
#[derive(Encode, Decode)]
pub(crate) enum DmDeviceReceipt {
    /// Exact intent and authenticated original public announcement.
    V1 {
        request: DmDeviceRequest,
        announcement: Announcement,
        event: SignedEvent,
    },
}

impl DmDeviceReceipt {
    fn resolve(&self, request: &DmDeviceRequest, db: &Database) -> DbResult<DmDeviceOutcome> {
        let Self::V1 {
            request: original,
            announcement,
            event,
        } = self;
        if original != request {
            return Err(DbError::DmDeviceRequestMismatch);
        }
        if event.kind() != EventKind::DM_DEVICE
            || event.is_singleton()
            || event.is_delete_parent_aux_content_set()
            || event.aux_key() != Default::default()
            || !match request {
                DmDeviceRequest::Retire { device_id } => {
                    announcement.device_id == *device_id && announcement.epoch.is_none()
                }
                DmDeviceRequest::Reenroll { expected } => {
                    announcement.epoch.is_some()
                        && !matches!(expected, DmEnrollmentExpectation::Retired { device_id }
                            if *device_id == announcement.device_id)
                }
            }
        {
            return Err(DbError::DmDeviceCandidate);
        }
        let event = VerifiedEvent::verify_signed(db.self_id, *event)
            .map_err(|_| DbError::DmDeviceCandidate)?;
        let raw = EventContentRaw::new(announcement.encode().context(DirectMessageSnafu)?);
        VerifiedEventContent::verify(event, raw).map_err(|_| DbError::DmDeviceCandidate)?;
        Ok(DmDeviceOutcome {
            device_id: announcement.device_id,
            event,
        })
    }
}

impl Database {
    /// Resolve an exact accepted retry without checking current lifecycle
    /// state.
    ///
    /// Trusted callers must additionally enforce full active-client authority
    /// using the calling session's secret. Database access is not UI authority.
    pub async fn dm_device_receipt(
        &self,
        secret: RostraIdSecretKey,
        id: DmDeviceRequestId,
        request: &DmDeviceRequest,
    ) -> DbResult<Option<DmDeviceOutcome>> {
        if secret.id() != self.self_id {
            return Err(DbError::DmDeviceCandidate);
        }
        self.read_with(|tx| {
            tx.open_table(&crate::ids_dm_device_receipts::TABLE)?
                .get(&id.0)?
                .map(|row| row.value_try()?.resolve(request, self))
                .transpose()
        })
        .await
    }

    /// Atomically sign and admit one conditional local lifecycle operation.
    ///
    /// This narrow trusted signing seam requires full active-client authority
    /// at the caller. It accepts no arbitrary callback. The caller reserves 148
    /// payload bytes before entry and holds allocation/buffer through
    /// completion. Receipt lookup, conditional state comparison, key
    /// generation, signing, admission, and journal insertion share one
    /// serialized transaction. A started synchronous transaction may commit
    /// despite caller cancellation.
    pub async fn commit_dm_device_request(
        &self,
        secret: RostraIdSecretKey,
        id: DmDeviceRequestId,
        request: &DmDeviceRequest,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
    ) -> DbResult<DmDeviceOutcome> {
        self.commit_dm_device_request_inner(
            secret,
            id,
            request,
            allocation,
            buffer,
            #[cfg(test)]
            None,
        )
        .await
    }

    pub(crate) async fn commit_dm_device_request_inner(
        &self,
        secret: RostraIdSecretKey,
        id: DmDeviceRequestId,
        request: &DmDeviceRequest,
        allocation: &mut Option<PayloadAllocation>,
        buffer: &mut Option<PayloadBuffer>,
        #[cfg(test)] hook: Option<crate::social_post_receipt::ReceiptTestHook>,
    ) -> DbResult<DmDeviceOutcome> {
        if secret.id() != self.self_id {
            return Err(DbError::DmDeviceCandidate);
        }
        self.write_with(|tx| {
            let mut receipts = tx.open_table(&crate::ids_dm_device_receipts::TABLE)?;
            if let Some(row) = receipts.get(&id.0)? {
                return row.value_try()?.resolve(request, self);
            }
            if Self::has_pending_migration_stash(tx)? {
                return Err(DbError::DmMigrationPending);
            }
            let now = Timestamp::now();
            let announcement = match request {
                DmDeviceRequest::Retire { device_id } => {
                    let state = tx
                        .open_table(&crate::ids_dm_devices::TABLE)?
                        .get(&(self.self_id, *device_id))?
                        .map(|row| row.value_try())
                        .transpose()?;
                    if state.is_none_or(|state| state.retired() || state.latest().is_none()) {
                        return Err(DbError::DmDeviceStateConflict);
                    }
                    Announcement {
                        device_id: *device_id,
                        epoch: None,
                    }
                }
                DmDeviceRequest::Reenroll { expected } => {
                    let mut installations = tx.open_table(&crate::ids_dm_installation::TABLE)?;
                    let current = installations
                        .get(&())?
                        .map(|row| row.value_try())
                        .transpose()?;
                    let matches = match (expected, &current) {
                        (DmEnrollmentExpectation::Absent, None) => true,
                        (DmEnrollmentExpectation::Retired { device_id }, Some(current)) => {
                            current.retired && current.device_id == *device_id
                        }
                        _ => false,
                    };
                    if !matches {
                        return Err(DbError::DmDeviceStateConflict);
                    }
                    let device_id = rand::random();
                    if current
                        .as_ref()
                        .is_some_and(|current| current.device_id == device_id)
                        || tx
                            .open_table(&crate::ids_dm_devices::TABLE)?
                            .get(&(self.self_id, device_id))?
                            .is_some()
                        || tx
                            .open_table(&crate::ids_dm_epochs::TABLE)?
                            .range((device_id, [0; 32])..=(device_id, [255; 32]))?
                            .next()
                            .transpose()?
                            .is_some()
                    {
                        return Err(DbError::DmDeviceCandidate);
                    }
                    let epoch = LocalEpoch::generate(now.as_u64()).context(DirectMessageSnafu)?;
                    let public = epoch.public().clone();
                    tx.open_table(&crate::ids_dm_epochs::TABLE)?
                        .insert(&(device_id, public.public_key), &epoch)?;
                    installations.insert(
                        &(),
                        &Installation {
                            device_id,
                            retired: false,
                        },
                    )?;
                    Announcement {
                        device_id,
                        epoch: Some(public),
                    }
                }
            };
            let raw = EventContentRaw::new(announcement.encode().context(DirectMessageSnafu)?);
            let head =
                Self::read_head_tx(self.self_id, &tx.open_table(&crate::events_heads::TABLE)?)?;
            let signed = Event::builder_raw_content()
                .author(self.self_id)
                .kind(EventKind::DM_DEVICE)
                .timestamp(
                    now.to_offset_date_time()
                        .ok_or(DbError::DmDeviceCandidate)?,
                )
                .maybe_parent_prev(head)
                .content(&raw)
                .build()
                .signed_by(secret);
            let event = VerifiedEvent::verify_signed(self.self_id, signed)
                .map_err(|_| DbError::DmDeviceCandidate)?;
            let content =
                VerifiedEventContent::verify(event, raw).map_err(|_| DbError::DmDeviceCandidate)?;
            self.admit_local_candidate_tx(tx, &content, allocation, buffer)?;
            let device_id = announcement.device_id;
            receipts.insert(
                &id.0,
                &DmDeviceReceipt::V1 {
                    request: *request,
                    announcement,
                    event: signed,
                },
            )?;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook(tx)?;
            }
            Ok(DmDeviceOutcome { device_id, event })
        })
        .await
    }
}
