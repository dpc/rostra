use std::collections::BTreeMap;
use std::time::Duration;

use rostra_core::event::VerifiedEvent;
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_p2p::Connection;
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::{BoxedErrorResult, FmtCompact, WhateverResult};
use snafu::ResultExt as _;
use tracing::{debug, instrument, trace};

use crate::client::Client;
use crate::{ClientHandle, LOG_TARGET};

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
    #[instrument(skip(self), ret)]
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

            let mut connections = BTreeMap::new();
            let mut followers_by_followee = BTreeMap::new();

            loop {
                let (events, new_cursor) = db.paginate_missing_events_contents(cursor, 100).await;

                for (author_id, event_id) in events {
                    let _ = self
                        .get_event_from_followers(
                            &self.client,
                            author_id,
                            event_id,
                            &mut connections,
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

    async fn get_event_from_followers(
        &self,
        client: &ClientHandle,
        author_id: RostraId,
        event_id: ShortEventId,
        connections: &mut BTreeMap<RostraId, Connection>,
        followers_by_followee: &mut BTreeMap<RostraId, Vec<RostraId>>,
        db: &rostra_client_db::Database,
    ) -> BoxedErrorResult<()> {
        let followers = if let Some(followers) = followers_by_followee.get(&author_id) {
            followers
        } else {
            let followers = db.get_followers(author_id).await;
            followers_by_followee.insert(author_id, followers);

            followers_by_followee
                .get(&author_id)
                .expect("Just inserted")
        };

        for follower_id in followers.iter().chain([author_id, self.self_id].iter()) {
            if connections.get(follower_id).is_none() {
                let conn = client
                    .client_ref()
                    .boxed()?
                    .connect(*follower_id)
                    .await
                    .boxed()?;
                connections.insert(*follower_id, conn);
            }
            let conn = connections.get_mut(follower_id).expect("Must exist");

            debug!(target:  LOG_TARGET,
                author_id = %author_id,
                event_id = %event_id,
                follower_id = %follower_id,
                "Getting event content from a peer"
            );
            match Self::get_event_content(event_id, conn, db).await {
                Ok(true) => {
                    return Ok(());
                }
                Ok(false) => {}
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
        }
        Ok(())
    }

    async fn get_event_content(
        event_id: ShortEventId,
        conn: &mut rostra_p2p::Connection,
        storage: &rostra_client_db::Database,
    ) -> WhateverResult<bool> {
        let event = storage
            .get_event(event_id)
            .await
            .expect("If content is missing, must have event already");

        let event = VerifiedEvent::assume_verified_from_signed(event.signed);
        let content = conn
            .get_event_content(event)
            .await
            .whatever_context("Failed to download peer data")?;

        if let Some(content) = content {
            storage.process_event_content(&content).await;
            return Ok(true);
        }

        Ok(false)
    }
}
