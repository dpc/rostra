use std::collections::HashSet;
use std::sync::Arc;

use rostra_client_db::Database;
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use snafu::ResultExt as _;
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, trace, warn};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;

/// Fetches events when any ID gets a new head written to the database.
///
/// This task subscribes to new head notifications from the database
/// and fetches the corresponding events from followers.
///
/// Only processes heads from IDs in our web of trust (self, followees,
/// and extended followees).
pub struct NewHeadFetcher {
    client: crate::client::ClientHandle,
    db: Arc<Database>,
    self_id: RostraId,
    new_heads_rx: broadcast::Receiver<(RostraId, ShortEventId)>,
    self_followees_rx: tokio::sync::watch::Receiver<
        std::collections::HashMap<RostraId, rostra_client_db::IdsFolloweesRecord>,
    >,
    connections: ConnectionCache,
}

impl NewHeadFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting new head fetcher");
        Self {
            client: client.handle(),
            db: client.db().clone(),
            self_id: client.rostra_id(),
            new_heads_rx: client.new_heads_subscribe(),
            self_followees_rx: client.self_followees_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "new-head-fetcher", skip(self), ret)]
    pub async fn run(mut self) {
        // Initialize web of trust cache
        let mut web_of_trust = self.build_web_of_trust().await;
        debug!(
            target: LOG_TARGET,
            count = web_of_trust.len(),
            "Initialized web of trust cache"
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

                    // Check if author is in our web of trust
                    if !web_of_trust.contains(&author) {
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
                res = self.self_followees_rx.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "Followees channel closed, shutting down");
                        break;
                    }
                    // Rebuild web of trust when followees change
                    web_of_trust = self.build_web_of_trust().await;
                    debug!(
                        target: LOG_TARGET,
                        count = web_of_trust.len(),
                        "Updated web of trust cache"
                    );
                }
            }
        }
    }

    /// Build the web of trust set (self + followees + extended followees)
    async fn build_web_of_trust(&self) -> HashSet<RostraId> {
        let (followees, extended) = self.db.get_followees_extended(self.self_id).await;

        let mut web_of_trust = HashSet::with_capacity(1 + followees.len() + extended.len());
        web_of_trust.insert(self.self_id);
        web_of_trust.extend(followees.into_keys());
        web_of_trust.extend(extended);

        web_of_trust
    }

    async fn fetch_events_for_head(
        &self,
        author: RostraId,
        head: ShortEventId,
    ) -> rostra_util_error::WhateverResult<()> {
        let followers = self.db.get_followers(author).await;

        // Try each follower (plus author and self) to get the events
        for follower_id in followers.iter().chain([author, self.self_id].iter()) {
            let Ok(client) = self.client.client_ref().boxed() else {
                break;
            };

            let conn = match self.connections.get_or_connect(&client, *follower_id).await {
                Ok(conn) => conn,
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        %head,
                        follower_id = %follower_id.to_short(),
                        err = %err.fmt_compact(),
                        "Could not connect to fetch new head events"
                    );
                    continue;
                }
            };

            debug!(
                target: LOG_TARGET,
                author = %author.to_short(),
                %head,
                follower_id = %follower_id.to_short(),
                "Fetching events from peer for new head"
            );

            match crate::util::rpc::download_events_from_head(author, head, &conn, &self.db).await {
                Ok(true) => {
                    debug!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        %head,
                        follower_id = %follower_id.to_short(),
                        "Successfully fetched events for new head"
                    );
                    return Ok(());
                }
                Ok(false) => {
                    debug!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        %head,
                        follower_id = %follower_id.to_short(),
                        "No new events found from peer"
                    );
                }
                Err(err) => {
                    debug!(
                        target: LOG_TARGET,
                        author = %author.to_short(),
                        %head,
                        follower_id = %follower_id.to_short(),
                        err = %err.fmt_compact(),
                        "Error fetching events from peer"
                    );
                }
            }
        }

        Ok(())
    }
}
