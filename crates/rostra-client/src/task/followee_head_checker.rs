use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::{Database, WotData};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util::is_rostra_dev_mode_set;
use rostra_util_error::{BoxedErrorResult, FmtCompact};
use snafu::ResultExt as _;
use tokio::sync::{Mutex, watch};
use tracing::{debug, info, instrument, trace};

use crate::ClientRef;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
const LOG_TARGET: &str = "rostra::head_checker";

/// Shared follower cache for concurrent head checking
type SharedFollowerCache = Arc<Mutex<BTreeMap<RostraId, Vec<RostraId>>>>;

pub struct FolloweeHeadChecker {
    client: crate::client::ClientHandle,
    db: Arc<Database>,
    self_id: RostraId,
    wot_updated: watch::Receiver<Arc<WotData>>,
    check_for_updates_rx: watch::Receiver<()>,
    connections: ConnectionCache,
}

impl FolloweeHeadChecker {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head checking task" );
        Self {
            client: client.handle(),
            db: client.db().to_owned(),
            self_id: client.rostra_id(),
            wot_updated: client.self_wot_subscribe(),
            check_for_updates_rx: client.check_for_updates_tx_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    /// Run the thread
    #[instrument(name = "followee-head-checker", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(self) {
        let mut check_for_updates_rx = self.check_for_updates_rx.clone();
        let mut wot_updated = self.wot_updated.clone();
        let mut interval = tokio::time::interval(if is_rostra_dev_mode_set() {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(60)
        });
        loop {
            // Trigger on ticks or any change
            tokio::select! {
                _ = interval.tick() => (),
                res = wot_updated.changed() => {
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

            if self.client.db().is_err() {
                break;
            };

            let connections = &self.connections;
            let followers_cache: SharedFollowerCache = Arc::new(Mutex::new(BTreeMap::new()));

            // Use the cached WoT data - cheap Arc clone
            let wot = wot_updated.borrow().clone();

            for id in [self.self_id].into_iter().chain(wot.iter_all()) {
                let Some(client) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                trace!(target: LOG_TARGET, %id, "Checking id");

                let followers_cache = followers_cache.clone();

                tokio::join!(
                    async {
                        let res = self.check_for_new_head_pkarr(&client, id).await;
                        trace!(target: LOG_TARGET, %id, ?res, "pkarr check finished");
                        self.process_head_check_result(
                            "pkarr",
                            id,
                            res,
                            connections,
                            &followers_cache,
                        )
                        .await;
                    },
                    async {
                        let res = self.check_for_new_head_iroh(&client, id, connections).await;
                        trace!(target: LOG_TARGET, %id, ?res, "iroh check finished");
                        self.process_head_check_result(
                            "iroh",
                            id,
                            res,
                            connections,
                            &followers_cache,
                        )
                        .await;
                    },
                );

                trace!(target: LOG_TARGET, %id, "Checking id - done");
            }
        }
    }

    async fn process_head_check_result(
        &self,
        source: &'static str,
        id: RostraId,
        res: BoxedErrorResult<Option<ShortEventId>>,
        connections: &ConnectionCache,
        followers_cache: &SharedFollowerCache,
    ) {
        match res {
            Err(err) => {
                info!(target: LOG_TARGET, err = %err, id = %id.to_short(), %source, "Failed to check for updates");
            }
            Ok(None) => {
                info!(target: LOG_TARGET, id = %id.to_short(), %source, "No updates");
            }
            Ok(Some(head)) => {
                info!(target: LOG_TARGET, id = %id.to_short(), %source, "Has updates");
                if let Err(err) = self
                    .download_new_data(id, head, connections, followers_cache)
                    .await
                {
                    info!(target: LOG_TARGET, err = %(&*err).fmt_compact(), id = %id.to_short(), "Failed to download new data");
                }
            }
        }
    }

    async fn check_for_new_head_iroh(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
        connections: &ConnectionCache,
    ) -> BoxedErrorResult<Option<ShortEventId>> {
        let Ok(conn) = connections.get_or_connect(client, id).await else {
            return Ok(None);
        };

        let head = conn.get_head(id).await.boxed()?;
        let now = Timestamp::now();
        client
            .p2p_state()
            .update(id, |state| {
                state.last_head_check = Some(now);
                state.last_checked_head = head;
            })
            .await;

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

        let now = Timestamp::now();
        client
            .p2p_state()
            .update(id, |state| {
                state.last_pkarr_resolve = Some(now);
                state.last_pkarr_head = data.published.head;
            })
            .await;

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
        connections: &ConnectionCache,
        followers_cache: &SharedFollowerCache,
    ) -> BoxedErrorResult<()> {
        // Get or fetch followers, locking only briefly
        let followers: Vec<RostraId> = {
            let mut cache = followers_cache.lock().await;
            if let Some(followers) = cache.get(&rostra_id) {
                followers.clone()
            } else {
                let client = self.client.client_ref().boxed()?;
                let storage = client.db();
                let followers = storage.get_followers(rostra_id).await;
                cache.insert(rostra_id, followers.clone());
                followers
            }
        };

        let peers: Vec<RostraId> = followers
            .into_iter()
            .chain([rostra_id, self.self_id])
            .collect();

        let client_ref = self.client.client_ref().boxed()?;

        match crate::util::rpc::download_events_from_child(
            rostra_id,
            head,
            &self.client,
            connections,
            &peers,
            client_ref.db(),
        )
        .await
        {
            Ok(_) => {}
            Err(err) => {
                debug!(target: LOG_TARGET,
                    rostra_id = %rostra_id,
                    head = %head,
                    err = %err.fmt_compact(),
                    "Error downloading new data"
                );
            }
        }

        Ok(())
    }
}
