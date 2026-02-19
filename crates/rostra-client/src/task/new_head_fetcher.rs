use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};

use rostra_client_db::{Database, WotData};
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::FmtCompact as _;
use tokio::sync::{broadcast, watch};
use tracing::{debug, instrument, trace, warn};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

const NUM_WORKERS: usize = 8;

/// Work queue that coalesces pending author IDs and prevents
/// concurrent fetches for the same author.
struct WorkQueue {
    inner: Mutex<WorkQueueInner>,
    notify: watch::Sender<()>,
}

struct WorkQueueInner {
    pending: BTreeSet<RostraId>,
    in_progress: HashSet<RostraId>,
}

impl WorkQueue {
    fn new() -> (Arc<Self>, watch::Receiver<()>) {
        let (notify, notify_rx) = watch::channel(());
        let queue = Arc::new(Self {
            inner: Mutex::new(WorkQueueInner {
                pending: BTreeSet::new(),
                in_progress: HashSet::new(),
            }),
            notify,
        });
        (queue, notify_rx)
    }

    /// Add an author to the pending set and notify workers.
    fn enqueue(&self, id: RostraId) {
        self.inner.lock().expect("not poisoned").pending.insert(id);
        let _ = self.notify.send(());
    }

    /// Take an author from the pending set that is not already
    /// in progress. Returns `None` if no eligible work is available.
    fn take_work(&self) -> Option<RostraId> {
        let mut inner = self.inner.lock().expect("not poisoned");
        let id = inner
            .pending
            .iter()
            .find(|id| !inner.in_progress.contains(id))
            .copied()?;
        inner.pending.remove(&id);
        inner.in_progress.insert(id);
        Some(id)
    }

    /// Mark an author as no longer in progress and notify workers,
    /// since previously skipped pending items may now be eligible.
    fn complete_work(&self, id: &RostraId) {
        self.inner
            .lock()
            .expect("not poisoned")
            .in_progress
            .remove(id);
        let _ = self.notify.send(());
    }
}

/// Fetches events when any ID gets a new head written to the database.
///
/// This task subscribes to new head notifications from the database
/// and fetches the corresponding events from followers.
///
/// Only processes heads from IDs in our web of trust (self, followees,
/// and extended followees).
///
/// Uses a pool of worker tasks to fetch events in parallel. Incoming
/// author IDs are coalesced in a `BTreeSet`, so multiple rapid updates
/// for the same author result in a single fetch. At most one worker
/// handles a given author at a time.
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

        let (queue, notify_rx) = WorkQueue::new();

        // Spawn worker tasks
        for worker_id in 0..NUM_WORKERS {
            tokio::spawn(Self::worker(
                worker_id,
                queue.clone(),
                notify_rx.clone(),
                self.networking.clone(),
                self.db.clone(),
                self.self_id,
                self.connections.clone(),
            ));
        }

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

                    queue.enqueue(author);
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

        // queue.notify is dropped here, causing all workers to shut down
    }

    async fn worker(
        worker_id: usize,
        queue: Arc<WorkQueue>,
        mut notify_rx: watch::Receiver<()>,
        networking: Arc<ClientNetworking>,
        db: Arc<Database>,
        self_id: RostraId,
        connections: ConnectionCache,
    ) {
        loop {
            let Some(author) = queue.take_work() else {
                // No eligible work — wait for notification
                if notify_rx.changed().await.is_err() {
                    trace!(target: LOG_TARGET, worker_id, "Worker shutting down");
                    break;
                }
                continue;
            };

            let heads = db.get_heads(author).await;

            for head in heads {
                Self::fetch_events_for_head(author, head, &networking, &connections, self_id, &db)
                    .await;
            }

            queue.complete_work(&author);
        }
    }

    async fn fetch_events_for_head(
        author: RostraId,
        head: ShortEventId,
        networking: &ClientNetworking,
        connections: &ConnectionCache,
        self_id: RostraId,
        db: &Database,
    ) {
        let followers = db.get_followers(author).await;

        let peers: Vec<RostraId> = followers.into_iter().chain([author, self_id]).collect();

        match crate::util::rpc::download_events_from_child(
            author,
            head,
            networking,
            connections,
            &peers,
            db,
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
    }
}
