use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use rostra_core::event::{VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_util_error::{FmtCompact, WhateverResult};
use snafu::{whatever, ResultExt as _};
use tracing::{debug, info, instrument};

use crate::client::Client;
use crate::db::InsertEventOutcome;
use crate::storage::Storage;
use crate::ClientRef;
const LOG_TARGET: &str = "rostra::head_checker";

pub struct FolloweeHeadChecker {
    client: crate::client::ClientHandle,
    storage: Arc<Storage>,
    followee_updated: tokio::sync::watch::Receiver<Vec<RostraId>>,
    check_for_updates_rx: tokio::sync::watch::Receiver<()>,
}

impl FolloweeHeadChecker {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head checking task" );
        Self {
            client: client.handle(),
            storage: client
                .storage_opt()
                .expect("Must start followee head checker only on a client with storage"),

            followee_updated: client
                .self_followees_list_subscribe()
                .expect("Can't start folowee checker without storage"),
            check_for_updates_rx: client.check_for_updates_tx_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        let mut check_for_updates_rx = self.check_for_updates_rx.clone();
        let mut followee_updated = self.followee_updated.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(10 * 60));

        loop {
            // Trigger on ticks or any change
            tokio::select! {
                _ = interval.tick() => (),
                res = followee_updated.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
                res = check_for_updates_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
            }
            // read / mark everything as read
            let self_followees = followee_updated.borrow_and_update().clone();
            check_for_updates_rx.mark_unchanged();

            for followee in &self_followees {
                let Some(client) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                let head = match self.check_for_new_head(&client, *followee).await {
                    Err(err) => {
                        info!(target: LOG_TARGET, err = %err.fmt_compact(), id = %followee, "Failed to check for updates");
                        continue;
                    }
                    Ok(None) => {
                        info!(target: LOG_TARGET, id = %followee, "No updates");
                        continue;
                    }
                    Ok(Some(head)) => {
                        info!(target: LOG_TARGET, id = %followee, "Has updates");
                        head
                    }
                };

                if let Err(err) = self.download_new_data(&client, *followee, head).await {
                    info!(target: LOG_TARGET, err = %err.fmt_compact(), id = %followee, "Failed to download new data");
                }
            }
        }
    }

    async fn check_for_new_head(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
    ) -> WhateverResult<Option<ShortEventId>> {
        let data = client
            .resolve_id_data(id)
            .await
            .whatever_context("Could not resolve id published data")?;

        if let Some(head) = data.published.head {
            if self.storage.has_event(head).await {
                return Ok(None);
            } else {
                return Ok(Some(head));
            }
        }

        whatever!("No head published")
    }

    async fn download_new_data(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
        head: ShortEventId,
    ) -> WhateverResult<()> {
        let mut events = BinaryHeap::from([(0, head)]);

        let storage = client.storage().whatever_context("No storage")?;

        let conn = client
            .connect(id)
            .await
            .whatever_context("Failed to connect")?;

        while let Some((depth, event_id)) = events.pop() {
            let event = conn
                .get_event(event_id)
                .await
                .whatever_context("Failed to query peer")?;

            let Some(event) = event else {
                continue;
            };
            let event = VerifiedEvent::verify_received(id, event_id, event.event, event.sig)
                .whatever_context("Invalid event received")?;

            let (insert_outcome, process_state) = storage.process_event(&event).await;

            if let InsertEventOutcome::Inserted {
                missing_parents, ..
            } = insert_outcome
            {
                for missing in missing_parents {
                    events.push((depth + 1, missing));
                }
            }

            if storage.wants_content(event_id, process_state).await {
                let content = conn
                    .get_event_content(
                        event_id,
                        event.event.content_len.into(),
                        event.event.content_hash,
                    )
                    .await
                    .whatever_context("Failed to download peer data")?;

                if let Some(content) = content {
                    let verified_content = VerifiedEventContent::verify(event, content)
                        .expect("Bao transfer should guarantee correct content was received");
                    storage.process_event_content(&verified_content).await;
                }
            }
        }

        Ok(())
    }
}
