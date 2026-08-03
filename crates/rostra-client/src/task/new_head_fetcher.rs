use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};

use rostra_client_db::{CurrentState, Database, DbResult, WotData};
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;
use tracing::{debug, error, instrument, trace, warn};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

#[cfg(test)]
mod tests;

const NUM_WORKERS: usize = 8;
const MAX_PENDING_AUTHORS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeadReconcileOutcome {
    Complete,
    MoreCandidates,
    QueueFull,
}

/// Work queue that coalesces pending author IDs and prevents
/// concurrent fetches for the same author.
struct WorkQueue {
    inner: Mutex<WorkQueueInner>,
    notify: watch::Sender<()>,
    space_available: tokio::sync::Notify,
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
            space_available: tokio::sync::Notify::new(),
        });
        (queue, notify_rx)
    }

    /// Add an author to the pending set and notify workers.
    fn try_enqueue(&self, id: RostraId) -> bool {
        let mut inner = self.inner.lock().expect("not poisoned");
        if inner.pending.contains(&id) {
            return true;
        }
        if MAX_PENDING_AUTHORS <= inner.pending.len() {
            return false;
        }
        inner.pending.insert(id);
        drop(inner);
        let _ = self.notify.send(());
        true
    }

    /// Take an author from the pending set that is not already
    /// in progress. Returns `None` if no eligible work is available.
    fn take_work(self: &Arc<Self>) -> Option<WorkLease> {
        let mut inner = self.inner.lock().expect("not poisoned");
        let id = inner
            .pending
            .iter()
            .find(|id| !inner.in_progress.contains(id))
            .copied()?;
        inner.pending.remove(&id);
        inner.in_progress.insert(id);
        Some(WorkLease {
            queue: self.clone(),
            author: id,
        })
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
        self.space_available.notify_one();
    }
}

/// Releases exclusive ownership of an author on every worker exit path.
struct WorkLease {
    queue: Arc<WorkQueue>,
    author: RostraId,
}

impl Drop for WorkLease {
    fn drop(&mut self) {
        self.queue.complete_work(&self.author);
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
/// author IDs are coalesced in a bounded queue, so multiple rapid updates
/// for the same author result in a single fetch. Durable heads are reconciled
/// at startup, after notification loss, and after WoT changes. At most one
/// worker handles a given author at a time. A database failure in any worker
/// stops the parent and cancels all sibling workers.
pub struct NewHeadFetcher {
    networking: Arc<ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    new_heads_rx: broadcast::Receiver<(RostraId, ShortEventId)>,
    wot: CurrentState<Arc<WotData>>,
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
            wot: client.self_wot_subscribe(),
            connections: client.connection_cache().clone(),
        }
    }

    #[instrument(name = "new-head-fetcher", skip(self), fields(self_id = %self.self_id.fmt_short()), ret)]
    pub async fn run(mut self) {
        debug!(
            target: LOG_TARGET,
            count = self.wot.snapshot().len(),
            "Started with web of trust cache"
        );

        let (queue, notify_rx) = WorkQueue::new();

        let mut workers = JoinSet::new();
        for worker_id in 0..NUM_WORKERS {
            workers.spawn(Self::worker(
                worker_id,
                queue.clone(),
                notify_rx.clone(),
                self.networking.clone(),
                self.db.clone(),
                self.self_id,
                self.connections.clone(),
            ));
        }
        let mut reconcile_cursor = None;
        // Capture the WoT baseline before the startup scan. Any admission that
        // races with the scan remains visible through `changed()`.
        let mut previous_wot = self.wot.snapshot();
        let mut reconciliation_pending = matches!(
            self.advance_current_heads_reconciliation(&queue, &mut reconcile_cursor)
                .await,
            HeadReconcileOutcome::QueueFull
        );
        let mut reconcile_again = false;

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
                            warn!(target: LOG_TARGET, lagged = n, "New head fetcher missed notifications; reconciling durable heads");
                            if reconciliation_pending {
                                reconcile_again = true;
                            } else {
                                reconcile_cursor = None;
                                reconciliation_pending = matches!(
                                    self.advance_current_heads_reconciliation(
                                        &queue,
                                        &mut reconcile_cursor,
                                    ).await,
                                    HeadReconcileOutcome::QueueFull
                                );
                            }
                            continue;
                        }
                    };

                    trace!(target: LOG_TARGET, author = %author.to_short(), %head, "New head notification received");

                    // Check if author is in our web of trust using the cached WoT
                    let in_wot = {
                        let wot = self.wot.snapshot();
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

                    if !queue.try_enqueue(author) {
                        if reconciliation_pending {
                            reconcile_again = true;
                        } else {
                            reconcile_cursor = None;
                            reconciliation_pending = true;
                        }
                    }
                }
                res = self.wot.changed() => {
                    if res.is_err() {
                        debug!(target: LOG_TARGET, "WoT channel closed, shutting down");
                        break;
                    }
                    debug!(
                        target: LOG_TARGET,
                        count = self.wot.snapshot().len(),
                        "Web of trust cache updated"
                    );
                    let current_wot = self.wot.snapshot();
                    for author in newly_admitted_authors(
                        &previous_wot,
                        &current_wot,
                        self.self_id,
                    ) {
                        if !self.db.get_heads(author).await.is_empty()
                            && !queue.try_enqueue(author)
                        {
                            break;
                        }
                    }
                    previous_wot = current_wot;

                    // A watch publication can coalesce remove/re-add transitions
                    // that have no visible snapshot delta. Reconcile durable heads
                    // once per coalesced update, without restarting an active scan.
                    if reconciliation_pending {
                        reconcile_again = true;
                    } else {
                        reconcile_cursor = None;
                        reconciliation_pending = matches!(
                            self.advance_current_heads_reconciliation(
                                &queue,
                                &mut reconcile_cursor,
                            ).await,
                            HeadReconcileOutcome::QueueFull
                        );
                    }
                }
                () = queue.space_available.notified(), if reconciliation_pending => {
                    reconciliation_pending = matches!(
                        self.advance_current_heads_reconciliation(
                            &queue,
                            &mut reconcile_cursor,
                        ).await,
                        HeadReconcileOutcome::QueueFull
                    );
                    if !reconciliation_pending && reconcile_again {
                        reconcile_again = false;
                        reconcile_cursor = None;
                        reconciliation_pending = matches!(
                            self.advance_current_heads_reconciliation(
                                &queue,
                                &mut reconcile_cursor,
                            ).await,
                            HeadReconcileOutcome::QueueFull
                        );
                    }
                }
                worker = workers.join_next() => {
                    match worker {
                        Some(Ok(Ok(()))) => {
                            error!(target: LOG_TARGET, "New-head worker stopped unexpectedly; stopping fetcher");
                        }
                        Some(Ok(Err(err))) => {
                            error!(
                                target: LOG_TARGET,
                                err = %err,
                                "Database ingestion failed in new-head worker; stopping fetcher"
                            );
                        }
                        Some(Err(err)) => {
                            error!(
                                target: LOG_TARGET,
                                err = %err,
                                "New-head worker panicked or was cancelled; stopping fetcher"
                            );
                        }
                        None => {
                            error!(target: LOG_TARGET, "New-head worker pool stopped; stopping fetcher");
                        }
                    }
                    break;
                }
            }
        }

        workers.abort_all();
        while workers.join_next().await.is_some() {}
    }

    async fn advance_current_heads_reconciliation(
        &self,
        queue: &WorkQueue,
        cursor: &mut Option<RostraId>,
    ) -> HeadReconcileOutcome {
        loop {
            match self.reconcile_current_heads(queue, cursor).await {
                HeadReconcileOutcome::MoreCandidates => continue,
                outcome => return outcome,
            }
        }
    }

    async fn reconcile_current_heads(
        &self,
        queue: &WorkQueue,
        cursor: &mut Option<RostraId>,
    ) -> HeadReconcileOutcome {
        let authors = self
            .db
            .get_ids_with_heads(*cursor, MAX_PENDING_AUTHORS)
            .await;
        let reached_end = authors.len() < MAX_PENDING_AUTHORS;
        let wot = self.wot.snapshot();
        enqueue_reconciled_authors(queue, authors, reached_end, &wot, self.self_id, cursor)
    }

    async fn worker(
        worker_id: usize,
        queue: Arc<WorkQueue>,
        mut notify_rx: watch::Receiver<()>,
        networking: Arc<ClientNetworking>,
        db: Arc<Database>,
        self_id: RostraId,
        connections: ConnectionCache,
    ) -> DbResult<()> {
        loop {
            let Some(lease) = queue.take_work() else {
                // No eligible work — wait for notification
                if notify_rx.changed().await.is_err() {
                    trace!(target: LOG_TARGET, worker_id, "Worker shutting down");
                    break;
                }
                continue;
            };
            let author = lease.author;

            let heads = db.get_heads(author).await;

            for head in heads {
                if let Err(err) = Self::fetch_events_for_head(
                    author,
                    head,
                    &networking,
                    &connections,
                    self_id,
                    &db,
                )
                .await
                {
                    error!(
                        target: LOG_TARGET,
                        worker_id,
                        author = %author.to_short(),
                        %head,
                        err = %err,
                        "Database ingestion failed; stopping new-head worker"
                    );
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    async fn fetch_events_for_head(
        author: RostraId,
        head: ShortEventId,
        networking: &ClientNetworking,
        connections: &ConnectionCache,
        self_id: RostraId,
        db: &Database,
    ) -> DbResult<()> {
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
        .await?
        {
            true => {
                debug!(
                    target: LOG_TARGET,
                    author = %author.to_short(),
                    %head,
                    "Successfully fetched events for new head"
                );
            }
            false => {
                debug!(
                    target: LOG_TARGET,
                    author = %author.to_short(),
                    %head,
                    "No new events found from any peer"
                );
            }
        }
        Ok(())
    }
}

fn newly_admitted_authors<'a>(
    previous: &'a WotData,
    current: &'a WotData,
    self_id: RostraId,
) -> impl Iterator<Item = RostraId> + 'a {
    current
        .iter_all()
        .filter(move |author| !previous.contains(*author, self_id))
}

fn enqueue_reconciled_authors(
    queue: &WorkQueue,
    authors: Vec<RostraId>,
    reached_end: bool,
    wot: &WotData,
    self_id: RostraId,
    cursor: &mut Option<RostraId>,
) -> HeadReconcileOutcome {
    for author in authors {
        if wot.contains(author, self_id) && !queue.try_enqueue(author) {
            return HeadReconcileOutcome::QueueFull;
        }
        *cursor = Some(author);
    }
    if reached_end {
        *cursor = None;
        HeadReconcileOutcome::Complete
    } else {
        HeadReconcileOutcome::MoreCandidates
    }
}
