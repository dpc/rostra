use std::collections::BTreeMap;

use rostra_core::event::{EventExt as _, SignedEventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_p2p::Connection;
use rostra_util_error::{BoxedErrorResult, FmtCompact, WhateverResult};
use snafu::ResultExt as _;
use tracing::{debug, instrument, trace, warn};

use crate::client::Client;
use crate::{ClientHandle, LOG_TARGET};

#[derive(Clone)]
pub struct MissingEventFetcher {
    // Notablye, we want to shutdown when db disconnects, so let's not keep references to it here
    client: crate::client::ClientHandle,
    self_id: RostraId,
    ids_with_missing_events_rx: dedup_chan::Receiver<RostraId>,
}

impl MissingEventFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting missing event fetcher" );
        Self {
            client: client.handle(),
            self_id: client.rostra_id(),
            ids_with_missing_events_rx: client.ids_with_missing_events_subscribe(100),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        let mut ids_with_missing_events_rx = self.ids_with_missing_events_rx.clone();

        loop {
            let author_id = match ids_with_missing_events_rx.recv().await {
                Ok(id) => id,
                Err(dedup_chan::RecvError::Closed) => break,
                Err(dedup_chan::RecvError::Lagging) => {
                    warn!(target: LOG_TARGET, "Missing event fetcher missed some notifications");
                    continue;
                }
            };
            trace!(target: LOG_TARGET, "Woke up");

            let Ok(db) = self.client.db() else {
                break;
            };

            let followers = db.get_followers(author_id).await;
            let missing_events = db.get_missing_events_for_id(author_id).await;

            if missing_events.is_empty() {
                continue;
            }

            let mut connections = BTreeMap::new();

            for follower_id in followers.iter().chain([self.self_id].iter()) {
                for missing_event in &missing_events {
                    if db.has_event(*missing_event).await {
                        continue;
                    }
                    match self
                        .get_event_from(
                            &self.client,
                            author_id,
                            *missing_event,
                            *follower_id,
                            &mut connections,
                            &db,
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(err) => {
                            debug!(target:  LOG_TARGET,
                                author_id = %author_id,
                                event_id = %missing_event,
                                follower_id = %follower_id,
                                err = %(&*err).fmt_compact(),
                                "Error while getting id from a peer"
                            );
                            connections.remove(follower_id);
                        }
                    }
                }
            }
        }
    }

    async fn get_event_from(
        &self,
        client: &ClientHandle,
        author_id: RostraId,
        event_id: ShortEventId,
        follower_id: RostraId,
        connections: &mut BTreeMap<RostraId, Connection>,
        db: &rostra_client_db::Database,
    ) -> BoxedErrorResult<()> {
        if connections.get(&follower_id).is_none() {
            let conn = client
                .client_ref()
                .boxed()?
                .connect(follower_id)
                .await
                .boxed()?;
            connections.insert(follower_id, conn);
        }
        let conn = connections.get_mut(&follower_id).expect("Must exist");

        debug!(target:  LOG_TARGET,
            author_id = %author_id,
            event_id = %event_id,
            follower_id = %follower_id,
            "Getting event from a peer"
        );
        match self.get_event(author_id, event_id, conn, db).await {
            Ok(_) => {}
            Err(err) => {
                debug!(target:  LOG_TARGET,
                    author_id = %author_id,
                    event_id = %event_id,
                    follower_id = %follower_id,
                    err = %err.fmt_compact(),
                    "Error getting event from a peer"
                );
            }
        }

        Ok(())
    }

    async fn get_event(
        &self,
        author_id: RostraId,
        event_id: ShortEventId,
        conn: &mut rostra_p2p::Connection,
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

        let (_, process_state) = storage.process_event(&event).await;

        if storage.wants_content(event_id, process_state).await {
            let content = conn
                .get_event_content(event_id, event.content_len(), event.content_hash())
                .await
                .whatever_context("Failed to download peer data")?;

            if let Some(content) = content {
                let verified_content = VerifiedEventContent::verify(event, content)
                    .expect("Bao transfer should guarantee correct content was received");
                storage.process_event_content(&verified_content).await;
                return Ok(true);
            }
        }

        Ok(true)
    }
}
