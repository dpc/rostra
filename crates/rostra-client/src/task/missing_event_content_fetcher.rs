use std::collections::BTreeMap;
use std::time::Duration;

use rostra_core::Timestamp;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tracing::{debug, instrument, trace};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

/// Initial backoff for content fetch retries (60 seconds).
const INITIAL_CONTENT_FETCH_BACKOFF_SECS: u64 = 60;

/// Maximum backoff for content fetch retries (24 hours).
const MAX_CONTENT_FETCH_BACKOFF_SECS: u64 = 86400;

/// Calculate exponential backoff seconds for a given attempt count.
///
/// Uses `min(INITIAL * 1.5^(count-1), MAX)`.
fn calculate_backoff_secs(attempt_count: u16) -> u64 {
    if attempt_count == 0 {
        return 0;
    }
    let multiplier = 1.5_f64.powi(i32::from(attempt_count) - 1);
    let backoff = (INITIAL_CONTENT_FETCH_BACKOFF_SECS as f64 * multiplier) as u64;
    backoff.min(MAX_CONTENT_FETCH_BACKOFF_SECS)
}

#[derive(Clone)]
pub struct MissingEventContentFetcher {
    // Notably, we want to shutdown when db disconnects, so let's not keep references to it here
    client: crate::client::ClientHandle,
    networking: std::sync::Arc<ClientNetworking>,
    self_id: RostraId,
    connections: ConnectionCache,
}

impl MissingEventContentFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting missing event content fetcher" );
        Self {
            client: client.handle(),
            networking: client.networking().clone(),
            self_id: client.rostra_id(),
            connections: client.connection_cache().clone(),
        }
    }

    /// Run the fetcher loop.
    ///
    /// Instead of polling on a fixed interval, this fetcher is event-driven:
    /// - Peeks at the first entry in `events_content_missing` (sorted by
    ///   scheduled fetch time)
    /// - If due: attempts to fetch, on failure records backoff
    /// - If not yet due: sleeps until the scheduled time or a notification
    /// - If empty: waits for a notification that new missing content arrived
    #[instrument(name = "missing-event-content-fetcher", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(self) {
        let Ok(db) = self.client.db() else {
            return;
        };
        let notify = db.content_missing_notify();

        let mut followers_by_followee: BTreeMap<RostraId, Vec<RostraId>> = BTreeMap::new();

        loop {
            let Ok(db) = self.client.db() else {
                break;
            };

            let Some(next) = db.peek_next_missing_content().await else {
                // No missing content. Wait for notification.
                trace!(target: LOG_TARGET, "No missing content, waiting for notification");
                notify.notified().await;
                continue;
            };

            let now = Timestamp::now();
            if now < next.scheduled_time {
                // Not yet due. Sleep until scheduled time or notification.
                let wait_secs = next.scheduled_time.secs_since(now);
                trace!(
                    target: LOG_TARGET,
                    wait_secs,
                    event_id = %next.event_id.to_short(),
                    "Next content fetch not yet due, sleeping"
                );
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(wait_secs)) => {},
                    () = notify.notified() => {},
                }
                continue;
            }

            // Due now — try to fetch content
            let author_id = next.author;
            let event_id = next.event_id;

            let followers = if let Some(followers) = followers_by_followee.get(&author_id) {
                followers.clone()
            } else {
                let followers = db.get_followers(author_id).await;
                followers_by_followee.insert(author_id, followers.clone());
                followers
            };

            let peers: Vec<RostraId> = followers
                .into_iter()
                .chain([author_id, self.self_id])
                .collect();

            let fetch_succeeded = match crate::util::rpc::download_events_from_child(
                author_id,
                event_id,
                &self.networking,
                &self.connections,
                &peers,
                &db,
            )
            .await
            {
                Ok(true) => true,
                Ok(false) => {
                    debug!(
                        target: LOG_TARGET,
                        author_id = %author_id.to_short(),
                        event_id = %event_id.to_short(),
                        "Could not fetch missing content from any peer"
                    );
                    false
                }
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        author_id = %author_id.to_short(),
                        event_id = %event_id.to_short(),
                        err = %err.fmt_compact(),
                        "Error fetching missing content"
                    );
                    false
                }
            };

            if !fetch_succeeded {
                let attempted_at = Timestamp::now();
                let new_attempt_count = next.fetch_attempt_count.saturating_add(1);
                let backoff_secs = calculate_backoff_secs(new_attempt_count);
                let next_attempt_at = attempted_at.saturating_add_secs(backoff_secs);

                debug!(
                    target: LOG_TARGET,
                    event_id = %event_id.to_short(),
                    attempt = new_attempt_count,
                    backoff_secs,
                    "Scheduling next content fetch attempt"
                );

                db.record_failed_content_fetch(
                    event_id,
                    next.scheduled_time,
                    attempted_at,
                    next_attempt_at,
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_backoff_secs() {
        assert_eq!(calculate_backoff_secs(0), 0);
        assert_eq!(calculate_backoff_secs(1), 60); // 60 * 1.5^0
        assert_eq!(calculate_backoff_secs(2), 90); // 60 * 1.5^1
        assert_eq!(calculate_backoff_secs(3), 135); // 60 * 1.5^2
        assert_eq!(calculate_backoff_secs(4), 202); // 60 * 1.5^3

        // Should cap at MAX
        assert_eq!(calculate_backoff_secs(50), MAX_CONTENT_FETCH_BACKOFF_SECS);
        assert_eq!(calculate_backoff_secs(64), MAX_CONTENT_FETCH_BACKOFF_SECS);
    }
}
