use rostra_core::ShortEventId;
use rostra_core::event::{EventExt as _, SignedEventExt as _, VerifiedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::{FmtCompact as _, WhateverResult};
use snafu::ResultExt as _;
use tracing::{debug, instrument, trace, warn};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;

#[derive(Clone)]
pub struct MissingEventFetcher {
    // Notably, we want to shutdown when db disconnects, so let's not keep references to it here
    client: crate::client::ClientHandle,
    self_id: RostraId,
    ids_with_missing_events_rx: dedup_chan::Receiver<RostraId>,
    connections: ConnectionCache,
}

impl MissingEventFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting missing event fetcher" );
        Self {
            client: client.handle(),
            self_id: client.rostra_id(),
            ids_with_missing_events_rx: client.ids_with_missing_events_subscribe(100),
            connections: client.connection_cache().clone(),
        }
    }

    /// Run the thread
    #[instrument(name = "missing-event-fetcher", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(self) {
        // Get initial IDs from WoT
        let mut initial_ids: Vec<RostraId> = {
            let Ok(client) = self.client.client_ref() else {
                return;
            };
            let wot = client.self_wot_subscribe();
            let wot = wot.borrow();
            wot.iter_all().collect()
        };
        let mut ids_with_missing_events_rx = self.ids_with_missing_events_rx.clone();

        loop {
            let author_id = if let Some(initial_id) = initial_ids.pop() {
                initial_id
            } else {
                match ids_with_missing_events_rx.recv().await {
                    Ok(id) => id,
                    Err(dedup_chan::RecvError::Closed) => break,
                    Err(dedup_chan::RecvError::Lagging) => {
                        warn!(target: LOG_TARGET, "Missing event fetcher missed some notifications");
                        continue;
                    }
                }
            };
            trace!(target: LOG_TARGET, "Woke up");

            let Ok(db) = self.client.db() else {
                break;
            };

            let followers = db.get_followers(author_id).await;
            let missing_events = db.get_missing_events_for_id(author_id).await;

            debug!(target: LOG_TARGET, len=missing_events.len(), id=%author_id.to_short(), "Missing events for id");
            if missing_events.is_empty() {
                continue;
            }

            let connections = &self.connections;

            for follower_id in followers.iter().chain([self.self_id].iter()) {
                let Ok(client) = self.client.client_ref().boxed() else {
                    break;
                };
                debug!(
                    target:  LOG_TARGET,
                    author_id = %author_id,
                    follower_id = %follower_id,
                    "Looking for a missing events from"
                );
                let Ok(conn) = connections.get_or_connect(&client, *follower_id).await else {
                    debug!(
                        target:  LOG_TARGET,
                        author_id = %author_id,
                        follower_id = %follower_id,
                        "Could not connect"
                    );
                    continue;
                };

                for missing_event in &missing_events {
                    if db.has_event(*missing_event).await {
                        continue;
                    }
                    match self.get_event(author_id, *missing_event, &conn, &db).await {
                        Ok(_) => {}
                        Err(err) => {
                            debug!(
                                target:  LOG_TARGET,
                                author_id = %author_id,
                                event_id = %missing_event,
                                follower_id = %follower_id,
                                err = %err.fmt_compact(),
                                "Error getting event from a peer"
                            );
                        }
                    }
                }
            }
        }
    }

    async fn get_event(
        &self,
        author_id: RostraId,
        event_id: ShortEventId,
        conn: &Connection,
        storage: &rostra_client_db::Database,
    ) -> WhateverResult<bool> {
        let event = conn
            .get_event(author_id, event_id)
            .await
            .whatever_context("Failed to query peer")?;

        let Some(event) = event else {
            return Ok(false);
        };
        let event =
            VerifiedEvent::verify_response(author_id, event_id, *event.event(), event.sig())
                .whatever_context("Invalid event received")?;

        storage.process_event(&event).await;

        Ok(true)
    }
}
