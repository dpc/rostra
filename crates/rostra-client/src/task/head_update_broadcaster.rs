use std::collections::HashMap;
use std::sync::Arc;

use rostra_client_db::{Database, IdsFollowersRecord};
use rostra_core::ShortEventId;
use rostra_core::event::{EventContentRaw, SignedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::{FmtCompact, WhateverResult};
use snafu::ResultExt as _;
use tokio::sync::watch;
use tracing::{debug, instrument, trace, warn};

/// Arc-wrapped followers map for cheap cloning
type FollowersMap = Arc<HashMap<RostraId, IdsFollowersRecord>>;

use crate::ClientRef;
use crate::client::Client;
const LOG_TARGET: &str = "rostra::head_broadcaster";

pub struct HeadUpdateBroadcaster {
    client: crate::client::ClientHandle,
    db: Arc<Database>,
    self_id: RostraId,
    self_followers_rx: watch::Receiver<FollowersMap>,
    self_head_rx: watch::Receiver<Option<ShortEventId>>,
}

impl HeadUpdateBroadcaster {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head broadcasting task" );
        Self {
            client: client.handle(),
            db: client.db().to_owned(),
            self_id: client.rostra_id(),

            self_followers_rx: client.self_followers_subscribe(),
            self_head_rx: client.self_head_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(name = "head-update-broadcaster", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(self) {
        let mut self_followers_rx = self.self_followers_rx.clone();
        let mut self_head_rx = self.self_head_rx.clone();
        loop {
            tokio::select! {
                res = self_head_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
                res = self_followers_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
            }
            trace!(target: LOG_TARGET, "Woke up");

            let Some(head) = *self_head_rx.borrow() else {
                warn!(target: LOG_TARGET, "Empty head!?");
                continue;
            };

            let followers = self_followers_rx.borrow().clone();
            debug!(
                target: LOG_TARGET,
                event_id = %head.to_short(),
                followers_num = followers.len(),
                "Broadcasting new head event to followers"
            );

            let Some(event) = self.db.get_event(head).await else {
                warn!(target: LOG_TARGET, event_id = %head.to_short(), "No head event!?");
                continue;
            };
            let Some(event_content) = self.db.get_event_content(head).await else {
                debug!(target: LOG_TARGET, event_id = %head.to_short(), "No head event content.");
                continue;
            };

            // send to ourselves first, in case we have redundant nodes
            for id in [self.self_id].into_iter().chain(followers.keys().copied()) {
                let Some(client) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                if let Err(err) = self
                    .broadcast_event(&client, id, &event.signed, &event_content)
                    .await
                {
                    debug!(
                        target: LOG_TARGET,
                        err = %err.fmt_compact(),
                        id = %id.to_short(),
                        "Failed to broadcast new head to node"
                    );
                }
            }
        }
    }

    async fn broadcast_event(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
        signed_event: &SignedEvent,
        event_content: &EventContentRaw,
    ) -> WhateverResult<()> {
        let conn = client
            .connect_cached(id)
            .await
            .whatever_context("Couldn't connect")?;

        conn.feed_event(*signed_event, event_content.clone())
            .await
            .whatever_context("Failed broadcasting head event")?;

        Ok(())
    }
}
