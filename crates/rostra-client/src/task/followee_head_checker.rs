use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::{Database, IdsFolloweesRecord, InsertEventOutcome};
use rostra_core::event::PersonaId;
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_p2p::connection::GetHeadRequest;
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::{BoxedErrorResult, FmtCompact, WhateverResult};
use rostra_util_fmt::AsFmtOption as _;
use snafu::ResultExt as _;
use tokio::sync::watch;
use tracing::{debug, info, instrument, trace};

use crate::client::Client;
use crate::ClientRef;
const LOG_TARGET: &str = "rostra::head_checker";

pub struct FolloweeHeadChecker {
    client: crate::client::ClientHandle,
    db: Arc<Database>,
    self_id: RostraId,
    followee_updated: watch::Receiver<HashMap<RostraId, IdsFolloweesRecord>>,
    check_for_updates_rx: watch::Receiver<()>,
}

impl FolloweeHeadChecker {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head checking task" );
        Self {
            client: client.handle(),
            db: client.db().to_owned(),
            self_id: client.rostra_id(),

            followee_updated: client.self_followees_subscribe(),
            check_for_updates_rx: client.check_for_updates_tx_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(name = "followee-head-checker", skip(self), ret)]
    pub async fn run(self) {
        let mut check_for_updates_rx = self.check_for_updates_rx.clone();
        let mut followee_updated = self.followee_updated.clone();
        let mut interval = tokio::time::interval(if is_rostra_dev_mode_set() {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(60)
        });
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
            trace!(target: LOG_TARGET, "Woke up");

            let Ok(storage) = self.client.db() else {
                break;
            };

            let self_followees = storage.get_self_followees().await;

            for (followee, _persona_id) in [&(self.self_id, PersonaId::default())]
                .into_iter()
                .chain(&self_followees)
            {
                let Some(client) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                let (head_pkarr, head_iroh) = tokio::join!(
                    self.check_for_new_head_pkarr(&client, *followee),
                    self.check_for_new_head_iroh(&client, *followee),
                );

                for (source, res) in [("pkarr", head_pkarr), ("iroh", head_iroh)] {
                    match res {
                        Err(err) => {
                            info!(target: LOG_TARGET, err = %err, id = %followee, %source, "Failed to check for updates");
                        }
                        Ok(None) => {
                            info!(target: LOG_TARGET, id = %followee, %source, "No updates");
                            continue;
                        }
                        Ok(Some(head)) => {
                            info!(target: LOG_TARGET, id = %followee, %source, "Has updates");
                            if let Err(err) = self.download_new_data(&client, *followee, head).await
                            {
                                info!(target: LOG_TARGET, err = %err.fmt_compact(), id = %followee, "Failed to download new data");
                            }
                        }
                    }
                }
            }
        }
    }

    async fn check_for_new_head_iroh(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
    ) -> BoxedErrorResult<Option<ShortEventId>> {
        let conn = client.connect(id).await?;

        let head = conn.make_rpc(&GetHeadRequest(id)).await.boxed()?;
        if let Some(head) = head.0 {
            if self.db.has_event(head).await {
                return Ok(None);
            } else {
                return Ok(Some(head));
            }
        }

        Ok(None)
    }

    async fn check_for_new_head_pkarr(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
    ) -> BoxedErrorResult<Option<ShortEventId>> {
        let data = client.resolve_id_data(id).await.boxed()?;

        if let Some(head) = data.published.head {
            if self.db.has_event(head).await {
                return Ok(None);
            } else {
                return Ok(Some(head));
            }
        }

        Ok(None)
    }

    async fn download_new_data(
        &self,
        client: &ClientRef<'_>,
        rostra_id: RostraId,
        head: ShortEventId,
    ) -> WhateverResult<()> {
        let mut events = BinaryHeap::from([(0, head)]);

        let storage = client.db();

        let conn = client
            .connect(rostra_id)
            .await
            .whatever_context("Failed to connect")?;

        let peer_id = conn.remote_node_id();

        while let Some((depth, event_id)) = events.pop() {
            debug!(
               target: LOG_TARGET,
                %depth,
                node_id = %peer_id.fmt_option(),
                %rostra_id,
                %event_id,
                "Querrying for event"
            );
            let Some(event) = conn
                .get_event(rostra_id, event_id)
                .await
                .whatever_context("Failed to query peer")?
            else {
                debug!(
                    target: LOG_TARGET,
                    %depth,
                    node_id = %peer_id.fmt_option(),
                    %rostra_id,
                    %event_id,
                    "Event not found"
                );
                continue;
            };
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
                    .get_event_content(event)
                    .await
                    .whatever_context("Failed to download peer data")?;

                if let Some(content) = content {
                    storage.process_event_content(&content).await;
                } else {
                    debug!(
                        target: LOG_TARGET,
                        %depth,
                        node_id = %peer_id.fmt_option(),
                        %rostra_id,
                        %event_id,
                        "Event content not found"
                    );
                }
            } else {
                debug!(
                    target: LOG_TARGET,
                    %rostra_id,
                    %event_id,
                    "Event content not wanted"
                );
            }
        }

        Ok(())
    }
}
