use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use rostra_core::Timestamp;
use rostra_core::id::RostraId;
use snafu::ResultExt as _;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::{Instant, timeout_at};

use super::Client;
use crate::error::{PostError, PostResult, StorageSnafu};

#[cfg(test)]
mod tests;

/// Local admission budget, not an I/O or publication completion deadline.
const WAIT_BUDGET: Duration = Duration::from_secs(2);

/// Serializes local follow mutations without retaining every historical target.
#[derive(Default)]
pub(super) struct FollowOrdering {
    /// Weak entries are pruned on each acquisition; waiters own strong entries.
    locks: Mutex<HashMap<RostraId, Weak<AsyncMutex<()>>>>,
}

/// Wall-clock sampling and waiting, replaceable by a deterministic test clock.
pub(super) trait FollowClock {
    fn now(&self) -> Timestamp;
    fn wait_next_second(&self) -> impl Future<Output = ()> + Send;
}

/// Production timestamps remain ordinary wall time, never logical time.
pub(super) struct WallClock;

impl FollowClock for WallClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    async fn wait_next_second(&self) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

impl FollowOrdering {
    fn lock_for(&self, followee: RostraId) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(&followee).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(followee, Arc::downgrade(&lock));
        lock
    }

    /// Hold the returned guard through ingestion, not merely timestamp
    /// selection.
    pub(super) async fn prepare(
        &self,
        client: &Client,
        followee: RostraId,
        clock: &impl FollowClock,
    ) -> PostResult<(OwnedMutexGuard<()>, Timestamp)> {
        let deadline = Instant::now() + WAIT_BUDGET;
        let acquired = timeout_at(deadline, self.lock_for(followee).lock_owned()).await;
        client.require_open_for_signing()?;
        let guard = acquired.map_err(|_| PostError::FollowOrderingBusy)?;
        loop {
            let latest = client
                .db
                .get_self_follow_timestamp(followee)
                .await
                .context(StorageSnafu)?;
            client.require_open_for_signing()?;
            let now = clock.now();
            if let Some(latest) = latest {
                if latest == Timestamp::MAX {
                    return Err(PostError::FollowTimestampExhausted);
                }
                if now < latest {
                    return Err(PostError::FollowClockBehind { latest, now });
                }
            }
            if deadline <= Instant::now() {
                return Err(PostError::FollowOrderingBusy);
            }
            if latest.is_none_or(|latest| latest < now) {
                return Ok((guard, now));
            }
            let waited = timeout_at(deadline, clock.wait_next_second()).await;
            client.require_open_for_signing()?;
            waited.map_err(|_| PostError::FollowOrderingBusy)?;
        }
    }
}
