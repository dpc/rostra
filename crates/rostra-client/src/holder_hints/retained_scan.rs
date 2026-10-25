use rostra_core::event::VerifiedEvent;

use super::{HolderHint, HolderHints, Key};

const HEADERS_PER_TURN: usize = 32;

/// One frozen snapshot, advanced fairly even when an early batch has no
/// changes.
pub(crate) struct RetainedHintScan {
    /// At most 1024 keys, each with at most two original observations.
    entries: Vec<(Key, Vec<HolderHint>)>,
    /// Next key; no secondary queue or accumulated previous snapshots.
    cursor: usize,
}

impl RetainedHintScan {
    pub(super) fn new(entries: Vec<(Key, Vec<HolderHint>)>) -> Self {
        Self { entries, cursor: 0 }
    }

    /// Read at most 32 bounded (1024-byte) headers, never payloads or network
    /// data. Return whether this frozen pass still has entries, not whether
    /// it changed.
    pub(crate) async fn step(
        &mut self,
        hints: &HolderHints,
        db: &rostra_client_db::Database,
    ) -> rostra_client_db::DbResult<bool> {
        let end = (self.cursor + HEADERS_PER_TURN).min(self.entries.len());
        while self.cursor < end {
            let (key, sources) = &self.entries[self.cursor];
            self.cursor += 1;
            if !sources.iter().any(|source| hints.is_live(*key, *source)) {
                continue;
            }
            let Some(record) = db.try_get_event(key.1).await? else {
                continue;
            };
            let Ok(event) = VerifiedEvent::verify_response(
                key.0,
                key.1,
                record.signed.event,
                record.signed.sig,
            ) else {
                continue;
            };
            // Storage reads may yield; expiry/eviction are checked again here.
            hints.propagate_checked(&event, sources);
        }
        Ok(self.cursor < self.entries.len())
    }
}
