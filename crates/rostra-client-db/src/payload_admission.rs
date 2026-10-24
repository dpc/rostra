//! Shared logical admission and separate buffer ownership.

use std::sync::Arc;

use redb::ReadableTable as _;
use rostra_core::event::{EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::ToShort as _;
use rostra_core::{ShortEventId, Timestamp};

use crate::event::ContentStoreRecord;
use crate::payload_reservation::{AdmissionState, ReservationOwner, ReservedEvent};
use crate::{
    Database, DbError, DbResult, EventContentState, PayloadAdmissionPause, PayloadAdmissionUsage,
    PayloadBuffer, PayloadReservation, WriteTransactionCtx, events_content_missing,
    events_content_state,
};

const CONTENT_HASH_ENCODED_LEN: usize = 32;

pub(crate) fn decode_stored_payload_after_length_guard<T>(
    encoded: &[u8],
    expected_len: u32,
    decode_owned: impl FnOnce(&[u8]) -> T,
) -> DbResult<T> {
    let (content, consumed) =
        bincode::borrow_decode_from_slice::<&[u8], _>(encoded, redb_bincode::BINCODE_CONFIG)?;
    if consumed != encoded.len() {
        return Err(
            bincode::error::DecodeError::Other("Trailing bytes after encoded value").into(),
        );
    }
    if content.len() != expected_len as usize {
        return Err(DbError::PayloadAccountingInvariant);
    }
    Ok(decode_owned(content))
}

pub(crate) fn encode_content_hash_key(
    hash: rostra_core::ContentHash,
) -> [u8; CONTENT_HASH_ENCODED_LEN] {
    let mut encoded = [0; CONTENT_HASH_ENCODED_LEN];
    let written = bincode::encode_into_slice(hash, &mut encoded, redb_bincode::BINCODE_CONFIG)
        .expect("fixed-size content hash must fit its database key buffer");
    assert_eq!(
        written, CONTENT_HASH_ENCODED_LEN,
        "content hash database key encoding must remain fixed-size"
    );
    encoded
}

/// Admission of one logical event, before any payload acquisition is started.
#[derive(Debug)]
pub enum PayloadReservationOutcome {
    /// Ordinary behavior: startup configuration leaves admission disabled.
    Disabled,
    /// This Missing event owns logical room; buffers must be reserved
    /// separately.
    Reserved(PayloadReservation),
    /// Temporary pause, not a download failure or a quota-pruned event.
    Deferred(PayloadAdmissionPause),
    /// Already materialized, empty or terminal; do not download.
    Unneeded,
}

/// Detailed preparation result for callers that account actual materialization.
#[derive(Debug)]
pub enum PayloadAcquisitionPreparation {
    /// Ordinary behavior: startup configuration leaves admission disabled.
    Disabled,
    /// This Missing event owns logical room; buffers must be reserved
    /// separately.
    Reserved(PayloadReservation),
    /// Temporary pause, not a download failure or a quota-pruned event.
    Deferred(PayloadAdmissionPause),
    /// Shared-store reuse materialized this event's payload.
    Materialized,
    /// Already materialized, empty, invalid, or terminal; do not download.
    Satisfied,
}

impl PayloadAcquisitionPreparation {
    fn into_legacy(self) -> PayloadReservationOutcome {
        match self {
            Self::Disabled => PayloadReservationOutcome::Disabled,
            Self::Reserved(reservation) => PayloadReservationOutcome::Reserved(reservation),
            Self::Deferred(reason) => PayloadReservationOutcome::Deferred(reason),
            Self::Materialized | Self::Satisfied => PayloadReservationOutcome::Unneeded,
        }
    }
}

/// Explicit content ingestion result for admission-aware callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadIngestOutcome {
    /// Ordinary validation and projection processing materialized this payload.
    Processed,
    /// Ordinary content validation classified this payload as Invalid.
    Invalid,
    /// Existing Processed/terminal/empty lifecycle did not need
    /// materialization.
    Unchanged,
    /// Header retained and Missing scheduling preserved; await a capacity
    /// wakeup.
    Deferred(PayloadAdmissionPause),
    /// Hash-store reuse found no locally stored bytes or no retained envelope.
    Unavailable,
}

impl Database {
    /// Retain the header, reuse stored bytes first, and reserve before
    /// fetching.
    ///
    /// Terminal and already materialized events never start network
    /// acquisition, including when admission limits are disabled. A
    /// temporary pause remains distinct from peer failure and never creates
    /// a quota decision.
    pub async fn prepare_payload_acquisition(
        &self,
        event: &VerifiedEvent,
    ) -> DbResult<PayloadReservationOutcome> {
        self.prepare_payload_acquisition_detailed(event)
            .await
            .map(PayloadAcquisitionPreparation::into_legacy)
    }

    /// Retain the header and report whether shared-store reuse materialized it.
    ///
    /// This preserves the reservation behavior of
    /// [`Self::prepare_payload_acquisition`] while distinguishing actual
    /// materialization from already-satisfied and terminal states.
    pub async fn prepare_payload_acquisition_detailed(
        &self,
        event: &VerifiedEvent,
    ) -> DbResult<PayloadAcquisitionPreparation> {
        if let Some(runtime) = &self.payload_runtime {
            return runtime.prepare(self, event).await;
        }
        self.prepare_payload_acquisition_once_detailed(event).await
    }

    /// One detailed allocation-free-on-return preparation attempt.
    pub(crate) async fn prepare_payload_acquisition_once_detailed(
        &self,
        event: &VerifiedEvent,
    ) -> DbResult<PayloadAcquisitionPreparation> {
        self.try_process_event(event).await?;
        match self
            .try_materialize_stored_payload(event.event_id.to_short())
            .await?
        {
            PayloadIngestOutcome::Unavailable => Ok(match self.reserve_payload(event).await? {
                PayloadReservationOutcome::Disabled => PayloadAcquisitionPreparation::Disabled,
                PayloadReservationOutcome::Reserved(reservation) => {
                    PayloadAcquisitionPreparation::Reserved(reservation)
                }
                PayloadReservationOutcome::Deferred(reason) => {
                    PayloadAcquisitionPreparation::Deferred(reason)
                }
                PayloadReservationOutcome::Unneeded => PayloadAcquisitionPreparation::Satisfied,
            }),
            PayloadIngestOutcome::Deferred(reason) => {
                Ok(PayloadAcquisitionPreparation::Deferred(reason))
            }
            PayloadIngestOutcome::Processed => Ok(PayloadAcquisitionPreparation::Materialized),
            PayloadIngestOutcome::Invalid | PayloadIngestOutcome::Unchanged => {
                Ok(PayloadAcquisitionPreparation::Satisfied)
            }
        }
    }

    /// Reserve one logical acquisition after retaining its verified envelope.
    ///
    /// No payload is downloaded/allocated and no quota pruning occurs. Ordinary
    /// envelope effects (including signed deletion and size rejection) still
    /// apply. A temporary refusal commits the header and leaves Missing
    /// resumable. Disabled is returned when no enforcing startup account is
    /// attached, including observation-only DryRun.
    pub async fn reserve_payload(
        &self,
        event: &VerifiedEvent,
    ) -> DbResult<PayloadReservationOutcome> {
        self.write_with(|tx| {
            self.process_event_tx(event, Timestamp::now(), tx)?;
            self.reserve_payload_tx(tx, event)
        })
        .await
    }

    pub(crate) fn reserve_payload_tx(
        &self,
        tx: &WriteTransactionCtx,
        event: &VerifiedEvent,
    ) -> DbResult<PayloadReservationOutcome> {
        let mut state = self.payload_admission.state.lock().unwrap();
        let Some(config) = &state.config else {
            return Ok(PayloadReservationOutcome::Disabled);
        };
        if !Self::payload_is_missing_tx(tx, event.event_id.to_short())? {
            return Ok(PayloadReservationOutcome::Unneeded);
        }
        let deferred = |pause| Ok(PayloadReservationOutcome::Deferred(pause));
        if state.events.contains_key(&event.event_id) {
            return deferred(PayloadAdmissionPause::AlreadyReserved);
        }
        if state.events.len() >= config.in_flight_count() {
            return deferred(PayloadAdmissionPause::InFlightCount);
        }
        let author = event.author();
        let bytes = u64::from(event.content_len());
        if let Some(pause) = Self::payload_capacity_pause_tx(tx, event, &state)? {
            return deferred(pause);
        }
        let id = state.next_id.checked_add(1).ok_or(DbError::Overflow)?;
        state.next_id = id;
        self.payload_admission.invalidate_pressure();
        state
            .events
            .insert(event.event_id, ReservedEvent { author, bytes, id });
        let ledger = self.payload_admission.clone();
        let event_id = event.event_id.to_short();
        tx.on_commit(move || {
            ledger.demands.lock().unwrap().remove_completed(event_id);
        });
        Ok(PayloadReservationOutcome::Reserved(PayloadReservation {
            inner: Arc::new(ReservationOwner {
                ledger: self.payload_admission.clone(),
                event: event.event_id,
                bytes,
                id,
            }),
        }))
    }

    pub(crate) fn payload_capacity_pause_tx(
        tx: &WriteTransactionCtx,
        event: &VerifiedEvent,
        state: &AdmissionState,
    ) -> DbResult<Option<PayloadAdmissionPause>> {
        let Some(config) = &state.config else {
            return Ok(None);
        };
        let Some(usage) = Self::payload_usage_tx(tx)? else {
            return Ok(Some(PayloadAdmissionPause::AccountingNotReady));
        };
        let author = event.author();
        let current_author = tx
            .open_table(&crate::ids_data_usage::TABLE)?
            .get(&author)?
            .map(|r| r.value().current_content_size)
            .unwrap_or(0);
        let bytes = u64::from(event.content_len());
        let mut global_reserved = 0u64;
        let mut author_reserved = 0u64;
        for (id, reservation) in &state.events {
            if *id == event.event_id {
                continue;
            }
            global_reserved = global_reserved
                .checked_add(reservation.bytes)
                .ok_or(DbError::Overflow)?;
            if reservation.author == author {
                author_reserved = author_reserved
                    .checked_add(reservation.bytes)
                    .ok_or(DbError::Overflow)?;
            }
        }
        if current_author
            .checked_add(author_reserved)
            .and_then(|n| n.checked_add(bytes))
            .is_none_or(|n| n > config.author_bytes(author))
        {
            return Ok(Some(PayloadAdmissionPause::AuthorCapacity));
        }
        if usage
            .logical_current_bytes
            .checked_add(global_reserved)
            .and_then(|n| n.checked_add(bytes))
            .is_none_or(|n| n > config.database_bytes())
        {
            return Ok(Some(PayloadAdmissionPause::DatabaseCapacity));
        }
        Ok(None)
    }

    /// Return independent reservation and owned-buffer counters.
    ///
    /// This advisory snapshot is not atomic with a separate persisted-usage
    /// query and must not authorize worker pruning outside the writer boundary.
    pub fn payload_admission_usage(&self) -> PayloadAdmissionUsage {
        let mut demands = self.payload_admission.demands.lock().unwrap();
        demands.expire(Timestamp::now());
        let (pending_demands, pending_demand_bytes) = demands.usage();
        let state = self.payload_admission.state.lock().unwrap();
        PayloadAdmissionUsage {
            acquisitions: state.events.len(),
            logical_reserved_bytes: state.events.values().map(|e| e.bytes).sum(),
            pending_demands,
            pending_demand_bytes,
            buffers: state.buffers,
            buffer_bytes: state.buffer_bytes,
        }
    }

    /// Observe guarded counters without expiring demands or changing ownership.
    ///
    /// Pending intent includes entries awaiting routine expiry. This bounded
    /// observation is separate from persisted accounting and grants no
    /// authority.
    pub fn payload_admission_observation(&self) -> PayloadAdmissionUsage {
        let demands = self.payload_admission.demands.lock().unwrap();
        let (pending_demands, pending_demand_bytes) = demands.usage();
        let state = self.payload_admission.state.lock().unwrap();
        PayloadAdmissionUsage {
            acquisitions: state.events.len(),
            logical_reserved_bytes: state.events.values().map(|e| e.bytes).sum(),
            pending_demands,
            pending_demand_bytes,
            buffers: state.buffers,
            buffer_bytes: state.buffer_bytes,
        }
    }

    /// Wait for a lossy capacity/lifecycle signal; register before checking
    /// work.
    ///
    /// This is not durable readiness. Future callers must also wake on config,
    /// startup and bounded retry/grace deadlines; a missed signal is not a
    /// latch.
    pub fn payload_admission_changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.payload_admission.changed.notified()
    }

    pub(crate) fn payload_is_missing_tx(
        tx: &WriteTransactionCtx,
        id: ShortEventId,
    ) -> DbResult<bool> {
        Ok(matches!(
            tx.open_table(&events_content_state::TABLE)?
                .get(&id)?
                .map(|r| r.value_try())
                .transpose()?,
            Some(EventContentState::Missing { .. })
        ))
    }

    /// Check the shared boundary before projections/bytes change.
    ///
    /// Legacy callers acquire an inline guard for their already-allocated
    /// bytes. Network callers must instead supply a guard acquired before
    /// the read.
    pub(crate) fn admit_materialization_tx(
        &self,
        tx: &WriteTransactionCtx,
        event: &VerifiedEvent,
        buffer: Option<&PayloadBuffer>,
    ) -> DbResult<Option<PayloadBuffer>> {
        if let Some(buffer) = buffer {
            let state = self.payload_admission.state.lock().unwrap();
            if !Arc::ptr_eq(&buffer.owner.ledger, &self.payload_admission)
                || buffer.owner.event != event.event_id
                || buffer.owner.bytes != u64::from(event.content_len())
                || !state
                    .events
                    .get(&event.event_id)
                    .is_some_and(|e| e.id == buffer.owner.id)
            {
                return Err(DbError::PayloadAdmissionPaused {
                    reason: PayloadAdmissionPause::ReservationExpired,
                });
            }
            if let Some(reason) = Self::payload_capacity_pause_tx(tx, event, &state)? {
                return Err(DbError::PayloadAdmissionPaused { reason });
            }
            return Ok(None);
        }
        match self.reserve_payload_tx(tx, event)? {
            PayloadReservationOutcome::Disabled | PayloadReservationOutcome::Unneeded => Ok(None),
            PayloadReservationOutcome::Deferred(reason) => {
                Err(DbError::PayloadAdmissionPaused { reason })
            }
            PayloadReservationOutcome::Reserved(reservation) => reservation
                .try_acquire_buffer()
                .map(Some)
                .map_err(|reason| DbError::PayloadAdmissionPaused { reason }),
        }
    }

    /// Ingest with explicit temporary outcomes, optionally using a pre-read
    /// lease.
    ///
    /// A Deferred result retains ordinary header effects without materializing
    /// this payload or making a quota-prune decision. Older fallible APIs
    /// instead return the typed `DbError::PayloadAdmissionPaused` and roll
    /// back the entire transaction.
    pub async fn try_process_admitted_event_content(
        &self,
        content: &VerifiedEventContent,
        buffer: Option<&PayloadBuffer>,
    ) -> DbResult<PayloadIngestOutcome> {
        self.write_with(|tx| {
            let now = Timestamp::now();
            self.process_event_tx(&content.event, now, tx)?;
            self.process_admitted_content_tx(tx, content, now, buffer)
        })
        .await
    }

    fn process_admitted_content_tx(
        &self,
        tx: &WriteTransactionCtx,
        content: &VerifiedEventContent,
        now: Timestamp,
        buffer: Option<&PayloadBuffer>,
    ) -> DbResult<PayloadIngestOutcome> {
        let missing = Self::payload_is_missing_tx(tx, content.event_id().to_short())?;
        match self.process_event_content_with_buffer_tx(content, now, tx, buffer) {
            Ok(()) => {
                if !missing {
                    return Ok(PayloadIngestOutcome::Unchanged);
                }
                let invalid = matches!(
                    tx.open_table(&events_content_state::TABLE)?
                        .get(&content.event_id().to_short())?
                        .map(|r| r.value_try())
                        .transpose()?,
                    Some(EventContentState::Invalid)
                );
                Ok(if invalid {
                    PayloadIngestOutcome::Invalid
                } else {
                    PayloadIngestOutcome::Processed
                })
            }
            Err(DbError::PayloadAdmissionPaused { reason }) => {
                self.ensure_missing_scheduled_tx(tx, content.event_id().to_short())?;
                Ok(PayloadIngestOutcome::Deferred(reason))
            }
            Err(err) => Err(err),
        }
    }

    /// Materialize hash-store bytes without fetching, through the same
    /// admission.
    ///
    /// Only this event is examined. A pause keeps its retry row, including when
    /// original insertion omitted that row because shared bytes already
    /// existed.
    pub async fn try_materialize_stored_payload(
        &self,
        id: ShortEventId,
    ) -> DbResult<PayloadIngestOutcome> {
        self.write_with(|tx| {
            let Some(event) = Self::get_event_tx(id, &tx.open_table(&crate::events::TABLE)?)?
            else {
                return Ok(PayloadIngestOutcome::Unavailable);
            };
            if !Self::payload_is_missing_tx(tx, id)? {
                return Ok(PayloadIngestOutcome::Unchanged);
            }
            let event = VerifiedEvent::assume_verified_from_signed(event.signed);
            // Acquire the buffer charge before copying the stored payload.
            let buffer = match self.admit_materialization_tx(tx, &event, None) {
                Ok(buffer) => buffer,
                Err(DbError::PayloadAdmissionPaused { reason }) => {
                    self.ensure_missing_scheduled_tx(tx, id)?;
                    return Ok(PayloadIngestOutcome::Deferred(reason));
                }
                Err(err) => return Err(err),
            };
            // Cow -> owned content currently copies Vec into Arc, temporarily
            // owning two payload allocations.
            let conversion = match self.reserve_payload_allocation(u64::from(event.content_len())) {
                Ok(capacity) => capacity,
                Err(reason) => {
                    self.ensure_missing_scheduled_tx(tx, id)?;
                    return Ok(PayloadIngestOutcome::Deferred(reason));
                }
            };
            let content_hash = event.content_hash();
            let encoded_key = encode_content_hash_key(content_hash);
            let content = {
                let content_store = tx.open_table(&crate::content_store::TABLE)?;
                let raw_content = content_store.as_raw().get(encoded_key.as_slice())?;
                let Some(raw_content) = raw_content else {
                    return Ok(PayloadIngestOutcome::Unavailable);
                };
                decode_stored_payload_after_length_guard(
                    raw_content.value(),
                    event.content_len(),
                    |_| -> DbResult<Option<_>> {
                        Ok(content_store
                            .get(&content_hash)?
                            .map(|record| -> Result<_, bincode::error::DecodeError> {
                                let ContentStoreRecord(content) = record.value_try()?;
                                Ok(content.into_owned())
                            })
                            .transpose()?)
                    },
                )??
            };
            let Some(content) = content else {
                return Ok(PayloadIngestOutcome::Unavailable);
            };
            drop(conversion);
            let verified = VerifiedEventContent::verify(event, content)
                .map_err(|_| DbError::PayloadAccountingInvariant)?;
            self.process_admitted_content_tx(tx, &verified, Timestamp::now(), buffer.as_ref())
        })
        .await
    }

    pub(crate) fn ensure_missing_scheduled_tx(
        &self,
        tx: &WriteTransactionCtx,
        id: ShortEventId,
    ) -> DbResult<()> {
        if let Some(EventContentState::Missing {
            next_fetch_attempt, ..
        }) = tx
            .open_table(&events_content_state::TABLE)?
            .get(&id)?
            .map(|r| r.value_try())
            .transpose()?
        {
            tx.open_table(&events_content_missing::TABLE)?
                .insert(&(next_fetch_attempt, id), &())?;
        }
        Ok(())
    }

    pub(crate) fn release_completed_admission_tx(
        &self,
        tx: &WriteTransactionCtx,
        id: ShortEventId,
    ) -> DbResult<()> {
        if self
            .payload_admission
            .state
            .lock()
            .unwrap()
            .config
            .is_none()
        {
            return Ok(());
        }
        if Self::payload_is_missing_tx(tx, id)? {
            return Ok(());
        }
        let ledger = self.payload_admission.clone();
        tx.on_commit(move || {
            let mut demands = ledger.demands.lock().unwrap();
            demands.remove_completed(id);
            let mut state = ledger.state.lock().unwrap();
            let previous = state.events.len();
            state.events.retain(|event, _| event.to_short() != id);
            if previous != state.events.len() {
                ledger.invalidate_pressure();
            }
            ledger.changed.notify_waiters();
        });
        Ok(())
    }
}
