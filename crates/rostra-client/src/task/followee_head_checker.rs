use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::{Database, IdsFolloweesRecord, InsertEventOutcome};
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::{BoxedErrorResult, FmtCompact, WhateverResult};
use rostra_util_fmt::AsFmtOption as _;
use snafu::ResultExt as _;
use tokio::sync::watch;
use tracing::{debug, info, instrument, trace};

use super::connection_cache::ConnectionCache;
use crate::ClientRef;
use crate::client::Client;
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

            let mut connections = ConnectionCache::new();
            let mut followers_by_followee = BTreeMap::new();

            let (followees_direct, followees_ext) =
                storage.get_followees_extended(self.self_id).await;

            for id in [self.self_id]
                .into_iter()
                .chain(followees_direct.into_keys())
                .chain(followees_ext)
            {
                let Some(client) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                let (head_pkarr, head_iroh) = tokio::join!(
                    self.check_for_new_head_pkarr(&client, id),
                    self.check_for_new_head_iroh(&client, id),
                );

                for (source, res) in [("pkarr", head_pkarr), ("iroh", head_iroh)] {
                    match res {
                        Err(err) => {
                            info!(target: LOG_TARGET, err = %err, id = %id.to_short(), %source, "Failed to check for updates");
                        }
                        Ok(None) => {
                            info!(target: LOG_TARGET, id = %id.to_short(), %source, "No updates");
                            continue;
                        }
                        Ok(Some(head)) => {
                            info!(target: LOG_TARGET, id = %id.to_short(), %source, "Has updates");
                            if let Err(err) = self
                                .download_new_data(
                                    id,
                                    head,
                                    &mut connections,
                                    &mut followers_by_followee,
                                )
                                .await
                            {
                                info!(target: LOG_TARGET, err = %(&*err).fmt_compact(), id = %id.to_short(), "Failed to download new data");
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

        let head = conn.get_head(id).await.boxed()?;
        if let Some(head) = head {
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
        rostra_id: RostraId,
        head: ShortEventId,
        connections: &mut ConnectionCache,
        followers_by_followee: &mut BTreeMap<RostraId, Vec<RostraId>>,
    ) -> BoxedErrorResult<()> {
        let followers = if let Some(followers) = followers_by_followee.get(&rostra_id) {
            followers
        } else {
            let client = self.client.client_ref().boxed()?;
            let storage = client.db();
            let followers = storage.get_followers(rostra_id).await;
            followers_by_followee.insert(rostra_id, followers);

            followers_by_followee
                .get(&rostra_id)
                .expect("Just inserted")
        };

        for follower_id in followers.iter().chain([rostra_id, self.self_id].iter()) {
            let Ok(client) = self.client.client_ref().boxed() else {
                break;
            };
            let Some(conn) = connections.get_or_connect(&client, *follower_id).await else {
                continue;
            };

            debug!(target: LOG_TARGET,
                rostra_id = %rostra_id,
                head = %head,
                follower_id = %follower_id,
                "Getting event data from a peer"
            );

            match self
                .download_new_data_from(&client, rostra_id, conn, head)
                .await
            {
                Ok(true) => {
                    return Ok(());
                }
                Ok(false) => {}
                Err(err) => {
                    debug!(target: LOG_TARGET,
                        rostra_id = %rostra_id,
                        head = %head,
                        follower_id = %follower_id,
                        err = %err.fmt_compact(),
                        "Error getting event from a peer"
                    );
                }
            }
        }
        Ok(())
    }
    async fn download_new_data_from(
        &self,
        client: &ClientRef<'_>,
        rostra_id: RostraId,
        conn: &mut Connection,
        head: ShortEventId,
    ) -> WhateverResult<bool> {
        let mut events = BinaryHeap::from([(0, head)]);
        let mut downloaded_anything = false;

        let storage = client.db();

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
            downloaded_anything = true;
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

        Ok(downloaded_anything)
    }
}
