use std::sync::Arc;
use std::time::Duration;

use rostra_core::id::{RostraId, ToShort as _};
use tracing::{debug, error, instrument, warn};

use crate::LOG_TARGET;
use crate::client::Client;
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

#[cfg(test)]
mod tests;

const RECONCILE_PAGE_SIZE: usize = 128;
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Eq, PartialEq)]
enum WakeReason {
    RetryDeadline,
    Notification(RostraId),
    Lagging,
    HolderNotified,
    HolderChanged,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingReconcileOutcome {
    Empty,
    WorkObserved,
}

#[derive(Clone, Copy, Debug)]
enum MissingReconcileTrigger {
    Startup,
    Lag,
    RetryDeadline,
}

struct MissingRetryPolicy {
    delay: Duration,
    deadline: Option<tokio::time::Instant>,
}

impl MissingRetryPolicy {
    fn new() -> Self {
        Self {
            delay: INITIAL_RETRY_DELAY,
            deadline: None,
        }
    }

    async fn reconcile<F, Fut>(
        &mut self,
        trigger: MissingReconcileTrigger,
        reconcile: F,
    ) -> rostra_client_db::DbResult<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = rostra_client_db::DbResult<MissingReconcileOutcome>>,
    {
        match reconcile().await? {
            MissingReconcileOutcome::Empty => {
                self.delay = INITIAL_RETRY_DELAY;
                self.deadline = None;
            }
            MissingReconcileOutcome::WorkObserved => {
                if matches!(trigger, MissingReconcileTrigger::RetryDeadline) {
                    self.delay = self.delay.saturating_mul(2).min(MAX_RETRY_DELAY);
                }
                if self.deadline.is_none()
                    || matches!(trigger, MissingReconcileTrigger::RetryDeadline)
                {
                    self.deadline = Some(tokio::time::Instant::now() + self.delay);
                }
            }
        }
        Ok(())
    }

    fn observe_notification_work(&mut self) {
        if self.deadline.is_none() {
            self.delay = INITIAL_RETRY_DELAY;
            self.deadline = Some(tokio::time::Instant::now() + self.delay);
        }
    }
}

#[derive(Clone)]
pub struct MissingEventFetcher {
    // Notably, we want to shutdown when db disconnects, so let's not keep references to it here
    client: crate::client::ClientHandle,
    networking: Arc<ClientNetworking>,
    self_id: RostraId,
    ids_with_missing_events_rx: dedup_chan::Receiver<RostraId>,
    connections: ConnectionCache,
}

impl MissingEventFetcher {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting missing event fetcher" );
        Self {
            client: client.handle(),
            networking: client.networking().clone(),
            self_id: client.rostra_id(),
            ids_with_missing_events_rx: client.ids_with_missing_events_subscribe(100),
            connections: client.connection_cache().clone(),
        }
    }

    /// Runs until client closure or a durable database ingestion failure.
    #[instrument(name = "missing-event-fetcher", skip(self), fields(self_id = %self.self_id.fmt_short()), ret)]
    pub async fn run(self) {
        let mut notifications = self.ids_with_missing_events_rx.clone();
        let mut retry = MissingRetryPolicy::new();
        let mut hint_scan: Option<crate::holder_hints::RetainedHintScan> = None;
        let mut next_hint_turn = None;

        let Ok(db) = self.client.db() else {
            return;
        };
        if let Err(err) = retry
            .reconcile(MissingReconcileTrigger::Startup, || {
                self.reconcile_missing_events(&db)
            })
            .await
        {
            error!(target: LOG_TARGET, err = %err, "Database ingestion failed; stopping missing-event fetcher");
            return;
        }

        loop {
            let reason = wait_for_work(
                &mut notifications,
                retry.deadline,
                &self.connections.holders.changed,
                next_hint_turn,
            )
            .await;
            if reason == WakeReason::HolderChanged {
                let scan =
                    hint_scan.get_or_insert_with(|| self.connections.holders.retained_scan());
                match scan.step(&self.connections.holders, &db).await {
                    Ok(true) => {}
                    Ok(false) => hint_scan = None,
                    Err(err) => {
                        error!(target: LOG_TARGET, err = %err, "Retained header read failed; stopping missing-event fetcher");
                        return;
                    }
                }
                next_hint_turn = hint_scan
                    .as_ref()
                    .map(|_| tokio::time::Instant::now() + INITIAL_RETRY_DELAY);
            }
            match reason {
                WakeReason::HolderNotified => {
                    // Persist the consumed wake before awaiting anything else.
                    // Ordinary notifications/retries cannot cancel this deadline.
                    next_hint_turn = Some(tokio::time::Instant::now() + INITIAL_RETRY_DELAY);
                }
                WakeReason::Notification(author) => {
                    let had_missing_events = !db.get_missing_events_for_id(author).await.is_empty();
                    if let Err(err) = self.fetch_missing_events(&db, author).await {
                        error!(target: LOG_TARGET, err = %err, "Database ingestion failed; stopping missing-event fetcher");
                        return;
                    }
                    if had_missing_events {
                        retry.observe_notification_work();
                    }
                }
                reason @ (WakeReason::Lagging | WakeReason::HolderChanged) => {
                    if reason == WakeReason::Lagging {
                        warn!(target: LOG_TARGET, "Missing event fetcher missed notifications; reconciling durable work");
                    }
                    if let Err(err) = retry
                        .reconcile(MissingReconcileTrigger::Lag, || {
                            self.reconcile_missing_events(&db)
                        })
                        .await
                    {
                        error!(target: LOG_TARGET, err = %err, "Database ingestion failed; stopping missing-event fetcher");
                        return;
                    }
                }
                WakeReason::RetryDeadline => {
                    if let Err(err) = retry
                        .reconcile(MissingReconcileTrigger::RetryDeadline, || {
                            self.reconcile_missing_events(&db)
                        })
                        .await
                    {
                        error!(target: LOG_TARGET, err = %err, "Database ingestion failed; stopping missing-event fetcher");
                        return;
                    }
                }
                WakeReason::Closed => break,
            }
        }
    }

    async fn reconcile_missing_events(
        &self,
        db: &rostra_client_db::Database,
    ) -> rostra_client_db::DbResult<MissingReconcileOutcome> {
        let Some(high_water) = db.get_last_id_with_missing_events().await else {
            return Ok(MissingReconcileOutcome::Empty);
        };
        let mut cursor = None;

        loop {
            let authors = db
                .get_ids_with_missing_events(cursor, RECONCILE_PAGE_SIZE)
                .await;
            let authors: Vec<_> = authors
                .into_iter()
                .take_while(|author| author <= &high_water)
                .collect();
            let Some(last_author) = authors.last().copied() else {
                break;
            };

            for author in authors {
                self.fetch_missing_events(db, author).await?;
            }
            if last_author == high_water {
                break;
            }
            cursor = Some(last_author);
        }

        Ok(MissingReconcileOutcome::WorkObserved)
    }

    async fn fetch_missing_events(
        &self,
        db: &rostra_client_db::Database,
        author_id: RostraId,
    ) -> rostra_client_db::DbResult<()> {
        let followers = db.get_followers(author_id).await;
        let missing_events = db.get_missing_events_for_id(author_id).await;

        debug!(target: LOG_TARGET, len=missing_events.len(), id=%author_id.to_short(), "Missing events for id");
        if missing_events.is_empty() {
            return Ok(());
        }

        let peers: Vec<_> = followers
            .into_iter()
            .chain([author_id, self.self_id])
            .collect();
        for missing_event in missing_events {
            if db.has_event(missing_event).await {
                continue;
            }
            if let Some((event, source)) = self
                .connections
                .get_event_with_source(&self.networking, &peers, author_id, missing_event)
                .await
            {
                let (inserted, _) = db.try_process_event(&event).await?;
                if let Some(source) = source {
                    self.connections
                        .holders
                        .inherit_retained(db, &event, source)
                        .await?;
                }
                self.connections
                    .holders
                    .inherit_known_retained(
                        db,
                        &event,
                        matches!(
                            inserted,
                            rostra_client_db::InsertEventOutcome::Inserted { .. }
                        ),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}

async fn wait_for_work(
    notifications: &mut dedup_chan::Receiver<RostraId>,
    retry_deadline: Option<tokio::time::Instant>,
    hints: &tokio::sync::Notify,
    hint_deadline: Option<tokio::time::Instant>,
) -> WakeReason {
    tokio::select! {
        reason = wait_for_retry_or_notification(notifications, retry_deadline) => reason,
        () = hints.notified(), if hint_deadline.is_none() => WakeReason::HolderNotified,
        () = async {
            match hint_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        } => WakeReason::HolderChanged,
    }
}

async fn wait_for_retry_or_notification(
    notifications: &mut dedup_chan::Receiver<RostraId>,
    retry_deadline: Option<tokio::time::Instant>,
) -> WakeReason {
    match retry_deadline {
        Some(deadline) => {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => WakeReason::RetryDeadline,
                result = notifications.recv() => match result {
                    Ok(author) => WakeReason::Notification(author),
                    Err(dedup_chan::RecvError::Lagging) => WakeReason::Lagging,
                    Err(dedup_chan::RecvError::Closed) => WakeReason::Closed,
                },
            }
        }
        None => match notifications.recv().await {
            Ok(author) => WakeReason::Notification(author),
            Err(dedup_chan::RecvError::Lagging) => WakeReason::Lagging,
            Err(dedup_chan::RecvError::Closed) => WakeReason::Closed,
        },
    }
}
