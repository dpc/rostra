//! Untrusted, ephemeral routes for already-wanted signed events.
//!
//! Restart drops these routes, even when missing-parent work is durable. They
//! never establish account ownership or replace signed endpoint announcements.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use rostra_core::ShortEventId;
use rostra_core::event::{EventExt as _, VerifiedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;

mod retained_scan;
pub(crate) use retained_scan::RetainedHintScan;

const MAX_KEYS: usize = 1024;
const MAX_HOLDERS: usize = 2;
const LIFETIME: Duration = Duration::from_secs(600);
pub(crate) const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

type Key = (RostraId, ShortEventId);

/// A transport observation, not an assertion of identity or possession.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HolderHint {
    /// Authenticated transport endpoint; no supplied addresses are retained.
    pub(crate) endpoint: iroh::EndpointId,
    /// Original observation deadline, inherited without extending it.
    expires: Instant,
}

impl HolderHint {
    /// Never let an operation outlive its inherited observation.
    pub(crate) fn timeout(self) -> Duration {
        self.expires
            .saturating_duration_since(Instant::now())
            .min(OPERATION_TIMEOUT)
    }
}

struct Holder {
    /// Original transport observation.
    hint: HolderHint,
    /// Retry suppression, also applied before an attempt can be cancelled.
    retry_at: Instant,
    /// Saturating exponential failure delay.
    delay: Duration,
    /// Prevent overlapping reads from bypassing this observation's cooldown.
    in_flight: bool,
}

/// Cancellation records failure cooldown from release, not from dial start.
pub(crate) struct HolderAttempt<'a> {
    /// Borrowed bounded state, never Client ownership.
    owner: &'a HolderHints,
    /// Exact requested key.
    key: Key,
    /// Observation and inherited expiry carried into successful ingestion.
    pub(crate) hint: HolderHint,
    /// This attempt's failure delay.
    delay: Duration,
    /// Set only after a verified response.
    successful: bool,
}

impl HolderAttempt<'_> {
    pub(crate) fn succeeded(mut self) {
        self.successful = true;
    }
}

impl Drop for HolderAttempt<'_> {
    fn drop(&mut self) {
        let mut entries = self
            .owner
            .entries
            .lock()
            .expect("holder hints mutex poisoned");
        if let Some(holder) = entries.get_mut(&self.key).and_then(|holders| {
            holders.iter_mut().find(|h| {
                h.hint.endpoint == self.hint.endpoint && h.hint.expires == self.hint.expires
            })
        }) {
            holder.in_flight = false;
            holder.retry_at = Instant::now()
                + if self.successful {
                    Duration::ZERO
                } else {
                    self.delay
                };
            if self.successful {
                holder.delay = Duration::from_secs(1);
            }
        }
    }
}

/// Bounded shared state; no task, connection, or Client ownership.
pub(crate) struct HolderHints {
    /// At most 1024 keys with two entries each; no eviction side queue.
    entries: Mutex<HashMap<Key, Vec<Holder>>>,
    /// One coalesced wake for the existing missing-header worker.
    pub(crate) changed: Notify,
    /// Holder operations share four non-waiting slots across all fetch paths.
    pub(crate) operations: Semaphore,
}

impl HolderHints {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            operations: Semaphore::new(4),
        }
    }

    /// Retained short-key collisions must not lend authority to another
    /// envelope.
    pub(crate) async fn observe_retained(
        &self,
        db: &rostra_client_db::Database,
        event: &VerifiedEvent,
        endpoint: iroh::EndpointId,
    ) -> rostra_client_db::DbResult<()> {
        if Self::matches_retained(db, event).await? {
            self.observe(event, endpoint);
        }
        Ok(())
    }

    /// Propagate only from the exact retained envelope, after successful
    /// ingestion.
    pub(crate) async fn inherit_retained(
        &self,
        db: &rostra_client_db::Database,
        event: &VerifiedEvent,
        hint: HolderHint,
    ) -> rostra_client_db::DbResult<()> {
        if self.is_live((event.author(), event.event_id.to_short()), hint)
            && Self::matches_retained(db, event).await?
            && self.is_live((event.author(), event.event_id.to_short()), hint)
        {
            self.inherit_inner(event, hint, true);
        }
        Ok(())
    }

    /// Ordinary-route wins and already-retained intermediates retain observed
    /// holder provenance; the winning account route is not new evidence.
    pub(crate) async fn inherit_known_retained(
        &self,
        db: &rostra_client_db::Database,
        event: &VerifiedEvent,
        inserted: bool,
    ) -> rostra_client_db::DbResult<()> {
        if !Self::matches_retained(db, event).await? {
            return Ok(());
        }
        let sources = self.sources((event.author(), event.event_id.to_short()));
        self.propagate_checked(event, &sources);
        if inserted && !sources.is_empty() {
            self.changed.notify_one();
        }
        Ok(())
    }

    async fn matches_retained(
        db: &rostra_client_db::Database,
        event: &VerifiedEvent,
    ) -> rostra_client_db::DbResult<bool> {
        Ok(db
            .try_get_event(event.event_id)
            .await?
            .is_some_and(|retained| {
                VerifiedEvent::verify_response(
                    event.author(),
                    event.event_id.to_short(),
                    retained.signed.event,
                    retained.signed.sig,
                )
                .is_ok_and(|retained| retained.event_id == event.event_id)
            }))
    }

    fn sources(&self, key: Key) -> Vec<HolderHint> {
        let entries = self.entries.lock().expect("holder hints mutex poisoned");
        entries
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|holder| Instant::now() < holder.hint.expires)
            .map(|holder| holder.hint)
            .collect()
    }

    fn is_live(&self, key: Key, hint: HolderHint) -> bool {
        self.sources(key)
            .iter()
            .any(|current| current.endpoint == hint.endpoint && current.expires == hint.expires)
    }

    fn propagate_checked(&self, event: &VerifiedEvent, sources: &[HolderHint]) {
        for hint in sources {
            self.inherit_inner(event, *hint, true);
        }
    }

    /// Freeze one bounded source snapshot; later changes coalesce through
    /// Notify.
    pub(crate) fn retained_scan(&self) -> RetainedHintScan {
        let entries = self.entries.lock().expect("holder hints mutex poisoned");
        RetainedHintScan::new(
            entries
                .iter()
                .map(|(key, holders)| {
                    (
                        *key,
                        holders
                            .iter()
                            .filter(|h| Instant::now() < h.hint.expires)
                            .map(|h| h.hint)
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    /// Record only after ordinary DB admission accepts the verified envelope.
    fn observe(&self, event: &VerifiedEvent, endpoint: iroh::EndpointId) {
        self.inherit(
            event,
            HolderHint {
                endpoint,
                expires: Instant::now() + LIFETIME,
            },
        );
    }

    /// Propagate only through a fetched, verified, DB-accepted ancestor.
    fn inherit(&self, event: &VerifiedEvent, hint: HolderHint) {
        self.inherit_inner(event, hint, false);
    }

    fn inherit_inner(&self, event: &VerifiedEvent, hint: HolderHint, require_source: bool) {
        let now = Instant::now();
        if hint.expires <= now {
            return;
        }
        let mut entries = self.entries.lock().expect("holder hints mutex poisoned");
        let key = (event.author(), event.event_id.to_short());
        let retained = entries
            .get(&key)
            .into_iter()
            .flatten()
            .find(|holder| holder.hint.endpoint == hint.endpoint && now < holder.hint.expires);
        if require_source && !retained.is_some_and(|holder| holder.hint.expires == hint.expires) {
            return;
        }
        // Clamp duplicate feeds before touching any descendant, even if its key
        // was evicted. Selection and insertion share this same lock.
        let hint = HolderHint {
            expires: retained.map_or(hint.expires, |holder| holder.hint.expires.min(hint.expires)),
            ..hint
        };
        entries.retain(|_, holders| {
            holders.retain(|holder| now < holder.hint.expires);
            !holders.is_empty()
        });
        let mut changed = false;
        for event_id in [
            Some(event.event_id.to_short()),
            event.parent_prev(),
            event.parent_aux(),
        ]
        .into_iter()
        .flatten()
        {
            let key = (event.author(), event_id);
            if !entries.contains_key(&key) && entries.len() == MAX_KEYS {
                let oldest = entries
                    .iter()
                    .min_by_key(|(_, holders)| holders[0].hint.expires)
                    .map(|(key, _)| *key)
                    .expect("full map has entries");
                entries.remove(&oldest);
            }
            let holders = entries.entry(key).or_default();
            if holders.iter().any(|h| h.hint.endpoint == hint.endpoint) {
                continue;
            }
            if holders.len() == MAX_HOLDERS {
                // A new authenticated source must not be excluded by two stale
                // or poisoned observations. Prefer replacing the most penalized.
                let replace = holders
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, h)| (h.delay, std::cmp::Reverse(h.hint.expires)))
                    .map(|(index, _)| index)
                    .expect("full holder list");
                holders.remove(replace);
            }
            holders.push(Holder {
                hint,
                retry_at: now,
                delay: Duration::from_secs(1),
                in_flight: false,
            });
            changed = true;
        }
        drop(entries);
        if changed {
            self.changed.notify_one();
        }
    }

    /// Choose one ready holder and penalize before awaiting, including
    /// cancellation.
    pub(crate) fn attempt(
        &self,
        author: RostraId,
        event: ShortEventId,
    ) -> Option<HolderAttempt<'_>> {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("holder hints mutex poisoned");
        let holders = entries.get_mut(&(author, event))?;
        let index = holders
            .iter()
            .position(|h| !h.in_flight && now < h.hint.expires && h.retry_at <= now)?;
        let mut holder = holders.remove(index);
        let delay = holder.delay;
        holder.in_flight = true;
        holder.retry_at = now + holder.delay;
        holder.delay = holder.delay.saturating_mul(2).min(Duration::from_secs(60));
        let hint = holder.hint;
        holders.push(holder);
        Some(HolderAttempt {
            owner: self,
            key: (author, event),
            hint,
            delay,
            successful: false,
        })
    }

    /// Model an immediately failed attempt in deterministic state tests.
    #[cfg(test)]
    pub(crate) fn take(&self, author: RostraId, event: ShortEventId) -> Option<HolderHint> {
        self.attempt(author, event).map(|attempt| attempt.hint)
    }
}

#[cfg(test)]
mod tests;
