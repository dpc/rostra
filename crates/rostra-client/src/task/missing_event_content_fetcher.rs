use std::collections::BTreeMap;
use std::time::Duration;

use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use rostra_util::is_rostra_dev_mode_set;
use tracing::{debug, instrument, trace};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
use crate::util::rpc::get_event_content_from_followers;

#[derive(Clone)]
pub struct MissingEventContentFetcher {
    // Notablye, we want to shutdown when db disconnects, so let's not keep references to it here
    client: crate::client::ClientHandle,
    self_id: RostraId,
}

impl MissingEventContentFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting missing event content fetcher" );
        Self {
            client: client.handle(),
            self_id: client.rostra_id(),
        }
    }

    /// Run the thread
    #[instrument(name = "missing-event-content-fetcher", skip(self), ret)]
    pub async fn run(self) {
        let mut interval = tokio::time::interval(if is_rostra_dev_mode_set() {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(600)
        });
        loop {
            // Trigger on ticks or any change
            tokio::select! {
                _ = interval.tick() => (),
            }
            trace!(target: LOG_TARGET, "Woke up");

            let Ok(db) = self.client.db() else {
                break;
            };

            let mut cursor: Option<ShortEventId> = None;

            let connections = ConnectionCache::new();
            let mut followers_by_followee = BTreeMap::new();

            loop {
                let (events, new_cursor) = db.paginate_missing_events_contents(cursor, 100).await;

                for (author_id, event_id) in events {
                    let _ = get_event_content_from_followers(
                        self.client.clone(),
                        self.self_id,
                        author_id,
                        event_id,
                        &connections,
                        &mut followers_by_followee,
                        &db,
                    )
                    .await;
                }

                cursor = if let Some(new_cursor) = new_cursor {
                    Some(new_cursor)
                } else {
                    break;
                }
            }
        }
    }
}
