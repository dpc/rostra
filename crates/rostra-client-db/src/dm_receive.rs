use std::ops::Bound;

use rostra_core::ShortEventId;
use rostra_core::event::{EventAuxKey, EventExt as _, EventKind, VerifiedEvent};
use rostra_dm::{MAX_TRIAL_KEYS, MessageBody};
use snafu::ResultExt as _;

use crate::dm::HistoryEntry;
use crate::{Database, DbResult, DirectMessageSnafu, WriteTransactionCtx};

impl Database {
    /// Atomically commit a locally encrypted message and its plaintext history.
    ///
    /// The trusted local publisher must encrypt this exact body into `content`
    /// and retain its allocation guard through this call. No sending-device
    /// envelope slot is available to reconstruct this history later. Admission
    /// refusal rolls back the entire event, not just its content. Retry the
    /// same signed event, never a new encryption of the same message
    /// identifier.
    pub async fn dm_commit_outgoing(
        &self,
        content: &rostra_core::event::VerifiedEventContent,
        body: &MessageBody,
        permit: &crate::dm::SendPermit,
        buffer: Option<&crate::PayloadBuffer>,
    ) -> DbResult<()> {
        body.validate_participants(self.self_id, self.self_id)
            .context(DirectMessageSnafu)?;
        if content.author() != self.self_id
            || content.kind() != EventKind::DIRECT_MESSAGE
            || content.event.is_singleton()
            || content.aux_key() != EventAuxKey::ZERO
        {
            return Err(rostra_dm::Error::Invalid).context(DirectMessageSnafu);
        }
        let bytes = content
            .content
            .as_ref()
            .ok_or(rostra_dm::Error::Invalid)
            .context(DirectMessageSnafu)?;
        rostra_dm::validate_frame(bytes.as_slice()).context(DirectMessageSnafu)?;
        self.write_with(|tx| {
            let now = rostra_core::Timestamp::now();
            if permit.sender != self.self_id
                || permit.recipient != body.recipient()
                || permit.send_until <= now.as_u64()
                || now.as_u64() < permit.send_from
            {
                return crate::DmRecipientUnavailableSnafu.fail();
            }
            let installation = tx
                .open_table(&crate::ids_dm_installation::TABLE)?
                .get(&())?
                .map(|row| row.value_try())
                .transpose()?;
            if installation
                .as_ref()
                .is_some_and(|installation| installation.retired)
                || installation.map(|installation| installation.device_id) != permit.sending_device
            {
                return crate::DmRecipientUnavailableSnafu.fail();
            }
            let devices = tx.open_table(&crate::ids_dm_devices::TABLE)?;
            for (key, selected) in &permit.selected {
                let current = devices.get(key)?.map(|row| row.value_try()).transpose()?;
                if current.as_ref() != Some(selected) {
                    return crate::DmRecipientUnavailableSnafu.fail();
                }
            }
            drop(devices);
            self.process_event_tx(&content.event, now, tx)?;
            self.process_event_content_with_buffer_tx(content, now, tx, buffer)?;
            use rostra_core::id::ToShort as _;
            let event_id = content.event_id().to_short();
            if tx
                .open_table(&crate::events_content_state::TABLE)?
                .get(&event_id)?
                .is_some()
            {
                return Err(rostra_dm::Error::Invalid).context(DirectMessageSnafu);
            }
            Self::dm_store_history_tx(tx, body, event_id, content.timestamp().as_u64())
        })
        .await
    }

    pub(crate) fn dm_store_history_tx(
        tx: &WriteTransactionCtx,
        body: &MessageBody,
        event_id: ShortEventId,
        timestamp: u64,
    ) -> DbResult<()> {
        let mut history = tx.open_table(&crate::events_dm_history::TABLE)?;
        let key = (body.sender(), body.message_id());
        let previous = history.get(&key)?.map(|row| row.value_try()).transpose()?;
        if let Some(mut previous) = previous {
            if previous.recipient != body.recipient() || previous.text != body.text() {
                previous.conflicted = true;
                history.insert(&key, &previous)?;
            }
        } else {
            let self_id = tx
                .open_table(&crate::ids_self::TABLE)?
                .get(&())?
                .ok_or(rostra_dm::Error::Invalid)
                .context(DirectMessageSnafu)?
                .value_try()?
                .rostra_id;
            tx.open_table(&crate::events_dm_history_by_conversation::TABLE)?
                .insert(
                    &(
                        body.sender().min(body.recipient()),
                        body.sender().max(body.recipient()),
                        timestamp,
                        event_id,
                    ),
                    &key,
                )?;
            history.insert(
                &key,
                &HistoryEntry {
                    sender: body.sender(),
                    recipient: body.recipient(),
                    message_id: body.message_id(),
                    text: body.text().to_owned(),
                    event_id,
                    timestamp,
                    conflicted: false,
                },
            )?;
            if body.recipient() == self_id {
                let mut incoming = tx.open_table(&crate::events_dm_incoming::TABLE)?;
                let sequence = incoming
                    .last()?
                    .map(|(sequence, _)| sequence.value_try())
                    .transpose()?
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(crate::DbError::Overflow)?;
                incoming.insert(&sequence, &key)?;
                tx.open_table(&crate::events_dm_incoming_by_peer::TABLE)?
                    .insert(&(body.sender(), sequence), &key)?;
                tx.open_table(&crate::events_dm_incoming_sequence::TABLE)?
                    .insert(&key, &sequence)?;
                let mut counts = tx.open_table(&crate::events_dm_incoming_count_by_peer::TABLE)?;
                let count = counts
                    .get(&body.sender())?
                    .map(|value| value.value_try())
                    .transpose()?
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(crate::DbError::Overflow)?;
                counts.insert(&body.sender(), &count)?;
            }
        }
        Ok(())
    }

    /// Perform one bounded background-only trial batch and commit its progress.
    ///
    /// Never invoke this on externally observable request paths. No decrypted
    /// content triggers network fetches, callbacks, or receipts to peers.
    /// Local trusted UI subscribers receive content-free progress wakeups for
    /// every serviced batch, independently of its decryption outcome. Expired
    /// live secrets are durably removed in a separate preceding
    /// transaction. All identities and trial plaintext stay inside the
    /// synchronous transaction closure and drop before this operation
    /// yields.
    ///
    /// Returns whether a queued row was serviced, not its decryption result.
    pub async fn dm_process_pending_now(&self) -> DbResult<bool> {
        self.dm_process_pending_with_clock(|| rostra_core::Timestamp::now().as_u64())
            .await
    }

    #[cfg(test)]
    pub(crate) async fn dm_process_pending(&self, now: u64) -> DbResult<bool> {
        self.dm_process_pending_with_clock(|| now).await
    }

    pub(crate) async fn dm_process_pending_with_clock(
        &self,
        clock: impl Fn() -> u64,
    ) -> DbResult<bool> {
        self.write_with(|tx| Self::dm_expire_keys_tx(tx, clock()))
            .await?;
        let worked = self
            .write_with(|tx| {
                let now = clock();
                // A lock wait or clock jump after the purge commit can expire more
                // keys. Yield to another durable purge rather than trying them or
                // combining deletion and decryption in one transaction.
                for row in tx.open_table(&crate::ids_dm_epochs::TABLE)?.range(..)? {
                    if row?.1.value_try()?.public().decrypt_until <= now {
                        return Ok(true);
                    }
                }
                let mut pending = tx.open_table(&crate::events_dm_pending::TABLE)?;
                let first = pending
                    .first()?
                    .map(|(key, value)| {
                        Ok::<_, crate::DbError>((key.value_try()?, value.value_try()?))
                    })
                    .transpose()?;
                let Some((event_id, cursor)) = first else {
                    return Ok(false);
                };
                let event = tx
                    .open_table(&crate::events::TABLE)?
                    .get(&event_id)?
                    .map(|row| row.value_try())
                    .transpose()?;
                let Some(event) = event else {
                    pending.remove(&event_id)?;
                    return Ok(true);
                };
                if event.kind() != EventKind::DIRECT_MESSAGE
                    || event.aux_key() != EventAuxKey::ZERO
                    || event.signed.is_singleton()
                    || event.content_len() as usize > rostra_dm::MAX_FRAME_BYTES
                    || tx
                        .open_table(&crate::events_content_state::TABLE)?
                        .get(&event_id)?
                        .is_some()
                {
                    pending.remove(&event_id)?;
                    return Ok(true);
                }
                // Reauthenticate the stored envelope at the crypto boundary. No
                // content lookup or trial can bypass the admitted event's lifecycle.
                if VerifiedEvent::verify_signed(event.author(), event.signed).is_err() {
                    pending.remove(&event_id)?;
                    return Ok(true);
                }
                let content = tx
                    .open_table(&crate::content_store::TABLE)?
                    .get(&event.content_hash())?
                    .map(|row| row.value_try())
                    .transpose()?;
                let Some(content) = content else {
                    pending.remove(&event_id)?;
                    return Ok(true);
                };
                let content = content.0.into_owned();
                if content.len() != event.content_len() as usize
                    || content.compute_content_hash() != event.content_hash()
                {
                    return Err(rostra_dm::Error::Invalid).context(DirectMessageSnafu);
                }
                let epochs = tx.open_table(&crate::ids_dm_epochs::TABLE)?;
                let mut identities = Vec::with_capacity(MAX_TRIAL_KEYS);
                let mut last = cursor;
                let start = cursor.map_or(Bound::Unbounded, Bound::Excluded);
                let mut remaining = epochs.range((start, Bound::Unbounded))?;
                for row in remaining.by_ref().take(MAX_TRIAL_KEYS) {
                    let (key, value) = row?;
                    let epoch = value.value_try()?;
                    identities.push(epoch.identity(now).context(DirectMessageSnafu)?);
                    last = Some(key.value_try()?);
                }
                let exhausted = remaining.next().transpose()?.is_none();
                if !identities.is_empty()
                    && let Ok(body) = rostra_dm::decrypt(
                        content.as_slice(),
                        &identities,
                        event.author(),
                        self.self_id,
                    )
                {
                    Self::dm_store_history_tx(tx, &body, event_id, event.timestamp().as_u64())?;
                    pending.remove(&event_id)?;
                } else if exhausted {
                    pending.remove(&event_id)?;
                } else {
                    pending.insert(&event_id, &last)?;
                }
                Ok(true)
            })
            .await?;
        if worked {
            self.dm_progress_tx.send_replace(());
        }
        Ok(worked)
    }
}
