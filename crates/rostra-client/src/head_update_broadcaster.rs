use std::collections::HashMap;
use std::sync::Arc;

use rostra_client_db::{Database, IdsFollowersRecord};
use rostra_core::event::{EventContent, EventExt as _, SignedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::ShortEventId;
use rostra_p2p::connection::{Connection, FeedEventRequest};
use rostra_util_error::{FmtCompact, WhateverResult};
use snafu::ResultExt as _;
use tokio::sync::watch;
use tracing::{debug, instrument, warn};

use crate::client::Client;
use crate::ClientRef;
const LOG_TARGET: &str = "rostra::head_broadcaster";

pub struct HeadUpdateBroadcaster {
    client: crate::client::ClientHandle,
    db: Arc<Database>,
    self_followers_rx: watch::Receiver<HashMap<RostraId, IdsFollowersRecord>>,
    self_head_rx: watch::Receiver<Option<ShortEventId>>,
}

impl HeadUpdateBroadcaster {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head checking task" );
        Self {
            client: client.handle(),
            db: client
                .storage_opt()
                .expect("Must start followee head checker only on a client with storage"),

            self_followers_rx: client
                .self_followers_subscribe()
                .expect("Can't start folowee checker without storage"),
            self_head_rx: client
                .self_head_subscribe()
                .expect("Can't start folowee checker without storage"),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
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
                warn!(target: LOG_TARGET, event_id = %head.to_short(), "No head event content!?");
                continue;
            };

            for (follower, _) in followers {
                let Some(client) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                if let Err(err) = self
                    .broadcast_event(&client, follower, &event.signed, &event_content)
                    .await
                {
                    debug!(
                        target: LOG_TARGET,
                        err = %err.fmt_compact(),
                        id = %follower.to_short(),
                        "Failed to broadcast new head to follower"
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
        event_content: &EventContent,
    ) -> WhateverResult<()> {
        let conn = client
            .connect(id)
            .await
            .whatever_context("Couldn't connect")?;

        conn.make_rpc_with_extra_data_send(&FeedEventRequest(*signed_event), |send| {
            Box::pin({
                let event_content = event_content.clone();
                let signed_event = *signed_event;
                async move {
                    Connection::write_bao_content(
                        send,
                        event_content.as_ref(),
                        signed_event.content_hash(),
                    )
                    .await?;
                    Ok(())
                }
            })
        })
        .await
        .whatever_context("Failed broadcasting head event")?;

        Ok(())
    }
}
