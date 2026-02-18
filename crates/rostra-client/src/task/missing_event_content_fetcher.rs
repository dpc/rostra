use std::collections::BTreeMap;
use std::time::Duration;

use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::FmtCompact as _;
use tracing::{debug, instrument, trace};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;

#[derive(Clone)]
pub struct MissingEventContentFetcher {
    // Notably, we want to shutdown when db disconnects, so let's not keep references to it here
    client: crate::client::ClientHandle,
    self_id: RostraId,
    connections: ConnectionCache,
}

impl MissingEventContentFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting missing event content fetcher" );
        Self {
            client: client.handle(),
            self_id: client.rostra_id(),
            connections: client.connection_cache().clone(),
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

            let connections = &self.connections;
            let mut followers_by_followee: BTreeMap<RostraId, Vec<RostraId>> = BTreeMap::new();

            loop {
                let (events, new_cursor) = db.paginate_missing_events_contents(cursor, 100).await;

                for (author_id, event_id) in events {
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

                    match crate::util::rpc::download_events_from_child(
                        author_id,
                        event_id,
                        &self.client,
                        connections,
                        &peers,
                        &db,
                    )
                    .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            debug!(target: LOG_TARGET,
                                author_id = %author_id.to_short(),
                                event_id = %event_id.to_short(),
                                "Could not fetch missing content from any peer"
                            );
                        }
                        Err(err) => {
                            debug!(target: LOG_TARGET,
                                author_id = %author_id.to_short(),
                                event_id = %event_id.to_short(),
                                err = %err.fmt_compact(),
                                "Error fetching missing content"
                            );
                        }
                    }
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
