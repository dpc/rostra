use std::time::Duration;

use rostra_core::Timestamp;
use snafu::ResultExt as _;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::{Instant, timeout_at};

use super::Client;
use super::follow_ordering::FollowClock;
use crate::error::{PostError, PostResult, StorageSnafu};

/// A local admission budget, not a database I/O or completion deadline.
const WAIT_BUDGET: Duration = Duration::from_secs(2);

/// Serializes full profile snapshots through their local ingestion.
#[derive(Default)]
pub(super) struct ProfileOrdering {
    /// One profile belongs to the client's identity.
    lock: Mutex<()>,
}

impl ProfileOrdering {
    /// Select actual wall time strictly after the durable profile projection.
    pub(super) async fn prepare(
        &self,
        client: &Client,
        clock: &impl FollowClock,
    ) -> PostResult<(MutexGuard<'_, ()>, Timestamp)> {
        let deadline = Instant::now() + WAIT_BUDGET;
        let acquired = timeout_at(deadline, self.lock.lock()).await;
        client.require_open_for_signing()?;
        let guard = acquired.map_err(|_| PostError::ProfileOrderingBusy)?;
        loop {
            let latest = client
                .db
                .get_self_profile_timestamp()
                .await
                .context(StorageSnafu)?;
            client.require_open_for_signing()?;
            let now = clock.now();
            if let Some(latest) = latest {
                if latest == Timestamp::MAX {
                    return Err(PostError::ProfileTimestampExhausted);
                }
                if now < latest {
                    return Err(PostError::ProfileClockBehind { latest, now });
                }
            }
            if deadline <= Instant::now() {
                return Err(PostError::ProfileOrderingBusy);
            }
            if latest.is_none_or(|latest| latest < now) {
                return Ok((guard, now));
            }
            let waited = timeout_at(deadline, clock.wait_next_second()).await;
            client.require_open_for_signing()?;
            waited.map_err(|_| PostError::ProfileOrderingBusy)?;
        }
    }
}

#[cfg(test)]
mod tests;
