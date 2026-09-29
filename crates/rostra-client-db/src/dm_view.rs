//! Bounded local views for session-authorized direct-message interfaces.

use std::ops::Bound;

use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use rostra_dm::DeviceState;
use snafu::ResultExt as _;

use crate::dm::HistoryEntry;
use crate::{Database, DbResult, DirectMessageSnafu};

/// Retained history paired with its local incoming-arrival sequence, when any.
pub struct SequencedHistoryEntry {
    /// Retained authenticated message.
    pub entry: HistoryEntry,
    /// Monotonic local sequence for incoming messages; outgoing messages have
    /// none.
    pub incoming_sequence: Option<u64>,
}

/// Public local installation metadata, without epoch secrets.
pub struct LocalInstallation {
    /// Stable random identifier, replaced only by explicit re-enrollment.
    pub device_id: [u8; 16],
    /// Whether this installation has permanently stopped sending.
    pub retired: bool,
}

impl Database {
    /// Subscribe to coalesced, content-free background trial-progress wakeups.
    /// These reveal neither the outcome nor the sender/recipient of a trial.
    pub fn dm_progress_subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.dm_progress_tx.subscribe()
    }

    /// Snapshot the latest local arrival sequence for one peer, independently
    /// of signed timestamps and session read markers.
    pub async fn dm_latest_incoming_sequence(&self, peer: RostraId) -> DbResult<u64> {
        self.read_with(|tx| {
            tx.open_table(&crate::events_dm_incoming_by_peer::TABLE)?
                .range((peer, 0)..=(peer, u64::MAX))?
                .next_back()
                .map(|row| {
                    let (key, _) = row?;
                    Ok(key.value_try()?.1)
                })
                .transpose()
                .map(|sequence| sequence.unwrap_or(0))
        })
        .await
    }

    /// Read a bounded oldest-arrival-first page after a local incoming cursor.
    /// Trusted callers must enforce full unlocked-session access.
    pub async fn dm_incoming_after(
        &self,
        peer: RostraId,
        after: u64,
        limit: usize,
    ) -> DbResult<Vec<SequencedHistoryEntry>> {
        self.read_with(|tx| {
            let index = tx.open_table(&crate::events_dm_incoming_by_peer::TABLE)?;
            let history = tx.open_table(&crate::events_dm_history::TABLE)?;
            let mut entries = Vec::new();
            for row in index
                .range((
                    Bound::Excluded((peer, after)),
                    Bound::Included((peer, u64::MAX)),
                ))?
                .take(limit.min(100))
            {
                let (key, value) = row?;
                let entry = history
                    .get(&value.value_try()?)?
                    .ok_or(rostra_dm::Error::Invalid)
                    .context(DirectMessageSnafu)?;
                entries.push(SequencedHistoryEntry {
                    entry: entry.value_try()?,
                    incoming_sequence: Some(key.value_try()?.1),
                });
            }
            Ok(entries)
        })
        .await
    }

    /// Read a bounded newest-first history page with local incoming sequences.
    pub async fn dm_history_with_sequences(
        &self,
        peer: RostraId,
        before: Option<(u64, ShortEventId)>,
        limit: usize,
    ) -> DbResult<Vec<SequencedHistoryEntry>> {
        let entries = self.dm_history_with(peer, before, limit).await?;
        self.read_with(|tx| {
            let sequences = tx.open_table(&crate::events_dm_incoming_sequence::TABLE)?;
            entries
                .into_iter()
                .map(|entry| {
                    let incoming_sequence = sequences
                        .get(&(entry.sender, entry.message_id))?
                        .map(|row| row.value_try())
                        .transpose()?;
                    Ok(SequencedHistoryEntry {
                        entry,
                        incoming_sequence,
                    })
                })
                .collect()
        })
        .await
    }

    /// Count exact unread incoming messages for one account-local browser
    /// session.
    pub async fn dm_count_unread(
        &self,
        session: [u8; 16],
        peer: Option<RostraId>,
        limit: usize,
    ) -> DbResult<usize> {
        self.read_with(|tx| {
            let (total, read) = if let Some(peer) = peer {
                let total = tx
                    .open_table(&crate::events_dm_incoming_count_by_peer::TABLE)?
                    .get(&peer)?
                    .map(|value| value.value_try())
                    .transpose()?
                    .unwrap_or(0);
                let read = tx
                    .open_table(&crate::ids_dm_read_count_by_peer::TABLE)?
                    .get(&(session, peer))?
                    .map(|value| value.value_try())
                    .transpose()?
                    .unwrap_or(0);
                (total, read)
            } else {
                let total = tx
                    .open_table(&crate::events_dm_incoming::TABLE)?
                    .last()?
                    .map(|(sequence, _)| sequence.value_try())
                    .transpose()?
                    .unwrap_or(0);
                let read = tx
                    .open_table(&crate::ids_dm_read_count::TABLE)?
                    .get(&(session, ()))?
                    .map(|value| value.value_try())
                    .transpose()?
                    .unwrap_or(0);
                (total, read)
            };
            Ok(total.saturating_sub(read).min(limit as u64) as usize)
        })
        .await
    }

    /// Atomically mark only the supplied incoming local-arrival sequences read.
    pub async fn dm_mark_read(&self, session: [u8; 16], sequences: &[u64]) -> DbResult<usize> {
        let sequences = sequences.iter().copied().take(100).collect::<Vec<_>>();
        self.write_with(|tx| {
            let incoming = tx.open_table(&crate::events_dm_incoming::TABLE)?;
            let history = tx.open_table(&crate::events_dm_history::TABLE)?;
            let mut markers = tx.open_table(&crate::ids_dm_read::TABLE)?;
            let mut total_counts = tx.open_table(&crate::ids_dm_read_count::TABLE)?;
            let mut peer_counts = tx.open_table(&crate::ids_dm_read_count_by_peer::TABLE)?;
            let mut marked = 0usize;
            for sequence in sequences {
                if markers.get(&(session, sequence))?.is_some() {
                    continue;
                }
                let key = incoming
                    .get(&sequence)?
                    .ok_or(rostra_dm::Error::Invalid)
                    .context(DirectMessageSnafu)?
                    .value_try()?;
                let peer = history
                    .get(&key)?
                    .ok_or(rostra_dm::Error::Invalid)
                    .context(DirectMessageSnafu)?
                    .value_try()?
                    .sender;
                markers.insert(&(session, sequence), &())?;
                let total = total_counts
                    .get(&(session, ()))?
                    .map(|value| value.value_try())
                    .transpose()?
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(crate::DbError::Overflow)?;
                total_counts.insert(&(session, ()), &total)?;
                let peer_count = peer_counts
                    .get(&(session, peer))?
                    .map(|value| value.value_try())
                    .transpose()?
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(crate::DbError::Overflow)?;
                peer_counts.insert(&(session, peer), &peer_count)?;
                marked += 1;
            }
            Ok(marked)
        })
        .await
    }

    /// Return at most 64 conversations in participant-pair order.
    ///
    /// Each conversation uses two index seeks, regardless of its history size.
    /// The exclusive cursor is the sorted participant pair of the last entry.
    /// Callers must require a full unlocked session before exposing plaintext.
    pub async fn dm_conversations(
        &self,
        after: Option<(RostraId, RostraId)>,
        limit: usize,
    ) -> DbResult<Vec<HistoryEntry>> {
        self.read_with(|tx| {
            let index = tx.open_table(&crate::events_dm_history_by_conversation::TABLE)?;
            let history = tx.open_table(&crate::events_dm_history::TABLE)?;
            let mut lower = after.map_or(Bound::Unbounded, |(first, second)| {
                Bound::Excluded((first, second, u64::MAX, ShortEventId::MAX))
            });
            let mut entries = Vec::new();
            for _ in 0..limit.min(64) {
                let Some(row) = index.range((lower, Bound::Unbounded))?.next() else {
                    break;
                };
                let (key, _) = row?;
                let (first, second, _, _) = key.value_try()?;
                let upper = (first, second, u64::MAX, ShortEventId::MAX);
                let (_, key) = index
                    .range((first, second, 0, ShortEventId::ZERO)..=upper)?
                    .next_back()
                    .ok_or(rostra_dm::Error::Invalid)
                    .context(DirectMessageSnafu)??;
                let entry = history
                    .get(&key.value_try()?)?
                    .ok_or(rostra_dm::Error::Invalid)
                    .context(DirectMessageSnafu)?
                    .value_try()?;
                entries.push(entry);
                lower = Bound::Excluded(upper);
            }
            Ok(entries)
        })
        .await
    }

    /// Read the current installation's public identity and retirement state.
    pub async fn dm_local_installation(&self) -> DbResult<Option<LocalInstallation>> {
        self.read_with(|tx| {
            Ok(tx
                .open_table(&crate::ids_dm_installation::TABLE)?
                .get(&())?
                .map(|row| {
                    row.value_try().map(|installation| LocalInstallation {
                        device_id: installation.device_id,
                        retired: installation.retired,
                    })
                })
                .transpose()?)
        })
        .await
    }

    /// Read at most 64 own-account device announcements in device-ID order.
    ///
    /// This includes permanent retirements and expired latest announcements;
    /// it never exposes local private epoch material.
    pub async fn dm_own_devices(
        &self,
        after: Option<[u8; 16]>,
        limit: usize,
    ) -> DbResult<Vec<([u8; 16], DeviceState)>> {
        self.read_with(|tx| {
            let lower = after.map_or(Bound::Included((self.self_id, [0; 16])), |device| {
                Bound::Excluded((self.self_id, device))
            });
            tx.open_table(&crate::ids_dm_devices::TABLE)?
                .range((lower, Bound::Included((self.self_id, [255; 16]))))?
                .take(limit.min(64))
                .map(|row| {
                    let (key, state) = row?;
                    Ok((key.value_try()?.1, state.value_try()?))
                })
                .collect()
        })
        .await
    }
}
