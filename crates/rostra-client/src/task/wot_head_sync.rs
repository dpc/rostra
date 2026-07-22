//! Periodic Web-of-Trust head synchronization.
//!
//! ## Why this task exists
//!
//! The other synchronization tasks cover specific scenarios:
//!
//! - **PollFolloweeHeadUpdates** maintains a long-lived blocking RPC
//!   (`WAIT_HEAD_UPDATE`) to each *direct followee*, so we learn about their
//!   new posts quickly.  However it only covers first-degree connections and
//!   only asks each peer about *their own* head.
//!
//! - **PollFollowerHeadUpdates** subscribes to `WAIT_FOLLOWERS_NEW_HEADS` on
//!   our *followers* (and self), receiving heads that those peers have heard
//!   about.  This is event-driven and fast, but it only relays what the
//!   follower already knows — if a follower never heard about an update for a
//!   2nd-degree ID, neither will we.
//!
//! Neither task performs a full sweep asking "what current head sample do you
//! have for every ID in my Web of Trust?" If a node was offline when an
//! update propagated, or the update came through a path that doesn't
//! reach any of our polled peers, we can end up permanently behind on the
//! current head set.
//!
//! ## What this task does
//!
//! On startup and then every hour, it iterates over every ID in the
//! current Web of Trust (self + direct followees + extended followees).
//! For each ID it asks that ID's known followers (plus the ID itself and
//! ourselves) for an independently sampled head via the lightweight `GET_HEAD`
//! RPC.
//! If any peer reports a head event we don't have locally, we call
//! `download_events_from_child` to fetch the full DAG — the same
//! function used by `NewHeadFetcher`. Repeated cycles can discover durable
//! sibling heads because each `GET_HEAD` response samples the peer's complete
//! current set; no individual response has complete-set semantics.
//!
//! Because this is a background maintenance sweep (not latency-critical),
//! it processes IDs sequentially and moves on after finding one new head
//! per ID, keeping resource usage low.

use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::{Database, DbResult, WotData};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tokio::sync::watch;
use tracing::{debug, error, instrument, trace};

use crate::client::{Client, ClientHandle};
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

const LOG_TARGET: &str = "rostra::wot_head_sync";
const SYNC_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub struct WotHeadSync {
    client: ClientHandle,
    networking: Arc<ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    wot_rx: watch::Receiver<Arc<WotData>>,
    connections: ConnectionCache,
}

impl WotHeadSync {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting WoT head sync task");
        Self {
            client: client.handle(),
            networking: client.networking().clone(),
            db: client.db().clone(),
            self_id: client.rostra_id(),
            wot_rx: client.self_wot_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "wot-head-sync", skip(self), fields(self_id = %self.self_id.fmt_short()), ret)]
    pub async fn run(self) {
        loop {
            if let Err(err) = self.sync_cycle().await {
                error!(
                    target: LOG_TARGET,
                    err = %err,
                    "Database ingestion failed; stopping WoT head sync"
                );
                return;
            }

            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            }

            debug!(target: LOG_TARGET, "Sync cycle complete, sleeping for {}s", SYNC_INTERVAL.as_secs());
            tokio::time::sleep(SYNC_INTERVAL).await;

            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                break;
            }
        }
    }

    async fn sync_cycle(&self) -> DbResult<()> {
        let wot_ids: Vec<RostraId> = {
            let wot = self.wot_rx.borrow();
            std::iter::once(self.self_id)
                .chain(wot.iter_all())
                .collect()
        };

        debug!(
            target: LOG_TARGET,
            wot_size = wot_ids.len(),
            "Starting WoT head sync cycle"
        );

        for id in &wot_ids {
            if self.client.app_ref_opt().is_none() {
                break;
            }

            self.sync_id(*id).await?;
        }
        Ok(())
    }

    async fn sync_id(&self, id: RostraId) -> DbResult<()> {
        let followers = self.db.get_followers(id).await;
        let peers: Vec<RostraId> = followers.into_iter().chain([id, self.self_id]).collect();

        for &peer_id in &peers {
            let conn = match self
                .connections
                .get_or_connect(&self.networking, peer_id)
                .await
            {
                Ok(conn) => conn,
                Err(_) => {
                    trace!(
                        target: LOG_TARGET,
                        id = %id.to_short(),
                        peer = %peer_id.to_short(),
                        "Could not connect to peer, skipping"
                    );
                    continue;
                }
            };

            let remote_head = match conn.get_head(id).await {
                Ok(Some(head)) => head,
                Ok(None) => continue,
                Err(err) => {
                    trace!(
                        target: LOG_TARGET,
                        id = %id.to_short(),
                        peer = %peer_id.to_short(),
                        err = %err.fmt_compact(),
                        "GET_HEAD failed, skipping peer"
                    );
                    continue;
                }
            };

            if self.db.has_event(remote_head).await {
                trace!(
                    target: LOG_TARGET,
                    id = %id.to_short(),
                    peer = %peer_id.to_short(),
                    head = %remote_head.to_short(),
                    "Head already known"
                );
                continue;
            }

            debug!(
                target: LOG_TARGET,
                id = %id.to_short(),
                peer = %peer_id.to_short(),
                head = %remote_head.to_short(),
                "Found unknown head, fetching events"
            );

            let download_result = crate::util::rpc::download_events_from_child(
                id,
                remote_head,
                &self.networking,
                &self.connections,
                &peers,
                &self.db,
            )
            .await;
            let downloaded = match download_result {
                Ok(downloaded) => downloaded,
                Err(err) => {
                    error!(
                        target: LOG_TARGET,
                        id = %id.to_short(),
                        peer_id = %peer_id.to_short(),
                        head = %remote_head.to_short(),
                        err = %err,
                        "Database ingestion failed while syncing a WoT head"
                    );
                    return Err(err);
                }
            };
            match downloaded {
                true => {
                    debug!(
                        target: LOG_TARGET,
                        id = %id.to_short(),
                        head = %remote_head.to_short(),
                        "Successfully fetched events for unknown head"
                    );
                }
                false => {
                    debug!(
                        target: LOG_TARGET,
                        id = %id.to_short(),
                        head = %remote_head.to_short(),
                        "No new events found from peers"
                    );
                }
            }

            // Found and processed an unknown head for this ID — move on
            break;
        }

        Ok(())
    }
}
