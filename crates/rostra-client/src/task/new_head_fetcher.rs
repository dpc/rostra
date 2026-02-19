use std::sync::Arc;

use rostra_client_db::{Database, WotData};
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tokio::sync::{broadcast, watch};
use tracing::{debug, info, instrument, trace, warn};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

/// Fetches events when any ID gets a new head written to the database.
///
/// This task subscribes to new head notifications from the database
/// and fetches the corresponding events from followers.
///
/// Only processes heads from IDs in our web of trust (self, followees,
/// and extended followees).
pub struct NewHeadFetcher {
    networking: Arc<ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    new_heads_rx: broadcast::Receiver<(RostraId, ShortEventId)>,
    wot_rx: watch::Receiver<Arc<WotData>>,
    connections: ConnectionCache,
}

impl NewHeadFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting new head fetcher");
        Self {
            networking: client.networking().clone(),
            db: client.db().clone(),
            self_id: client.rostra_id(),
            new_heads_rx: client.new_heads_subscribe(),
            wot_rx: client.self_wot_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "new-head-fetcher", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(mut self) {
        debug!(
            target: LOG_TARGET,
            count = self.wot_rx.borrow().len(),
            "Started with web of trust cache"
        );

        loop {
            tokio::select! {
                res = self.new_heads_rx.recv() => {
                    let (author, head) = match res {
                        Ok(msg) => msg,
                        Err(broadcast::error::RecvError::Closed) => {
                            debug!(target: LOG_TARGET, "New heads channel closed, shutting down");
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(target: LOG_TARGET, lagged = n, "New head fetcher missed some notifications");
                            continue;
                        }
                    };

                    trace!(target: LOG_TARGET, author = %author.to_short(), %head, "New head notification received");

                    // Check if author is in our web of trust using the cached WoT
                    // Clone the Arc to avoid holding the borrow across await
                    let in_wot = {
                        let wot = self.wot_rx.borrow();
                        wot.contains(author, self.self_id)
                    };

                    if !in_wot {
                        trace!(
                            target: LOG_TARGET,
                            author = %author.to_short(),
                            %head,
                            "Ignoring head from ID not in web of trust"
                        );
                        continue;
                    }

                    // Try to fetch events from followers of this author
                    if let Err(err) = self.fetch_events_for_head(author, head).await {
                        info!(
                            target: LOG_TARGET,
                            author = %author.to_short(),
                            %head,
                            err = %err.fmt_compact(),
                            "Failed to fetch events for new head"
                        );
                    }
                }
                res = self.wot_rx.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "WoT channel closed, shutting down");
                        break;
                    }
                    debug!(
                        target: LOG_TARGET,
                        count = self.wot_rx.borrow().len(),
                        "Web of trust cache updated"
                    );
                }
            }
        }
    }

    async fn fetch_events_for_head(
        &self,
        author: RostraId,
        head: ShortEventId,
    ) -> rostra_util_error::WhateverResult<()> {
        let followers = self.db.get_followers(author).await;

        let peers: Vec<RostraId> = followers
            .into_iter()
            .chain([author, self.self_id])
            .collect();

        match crate::util::rpc::download_events_from_child(
            author,
            head,
            &self.networking,
            &self.connections,
            &peers,
            &self.db,
        )
        .await
        {
            Ok(true) => {
                debug!(
                    target: LOG_TARGET,
                    author = %author.to_short(),
                    %head,
                    "Successfully fetched events for new head"
                );
            }
            Ok(false) => {
                debug!(
                    target: LOG_TARGET,
                    author = %author.to_short(),
                    %head,
                    "No new events found from any peer"
                );
            }
            Err(err) => {
                debug!(
                    target: LOG_TARGET,
                    author = %author.to_short(),
                    %head,
                    err = %err.fmt_compact(),
                    "Error fetching events for new head"
                );
            }
        }

        Ok(())
    }
}
