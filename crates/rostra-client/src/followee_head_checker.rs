use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use rostra_core::event::{SignedEvent, VerifiedEvent};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_p2p::connection::GetEventRequest;
use rostra_util_error::{BoxedErrorResult, FmtCompact, WhateverResult};
use snafu::{whatever, ResultExt as _, Whatever};
use tracing::{debug, info, instrument};

use crate::client::Client;
use crate::storage::Storage;
use crate::ClientRef;
const LOG_TARGET: &str = "rostra::client::head_checker";

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

        while let Some((_, event_id)) = events.pop() {
            let event = conn
                .make_rpc(&GetEventRequest(event_id))
                .await
                .whatever_context("Failed to query peer")?;

            let Some(event) = event.0 else {
                continue;
            };
            let event = VerifiedEvent::verify_received(id, event_id, event.event, event.sig)
                .whatever_context("Invalid event received")?;

            storage.process_event(&event).await;

            let (event, signed_event) = conn
                .make_rpc_with_extra_data_recv(&GetEventRequest(event_id), |recv, resp| {
                    let resp = resp.to_owned();
                    Box::pin(async move {
                        resp.0
                            .map(|SignedEvent { event, sig }| {
                                VerifiedEvent::verify_received(id, event_id, event, sig)
                                    .whatever_context("Invalid event received")
                            })
                            .transpose()
                    })
                })
                .await
                .whatever_context("Rpc failed")?;
        }

        Ok(())
    }
}
