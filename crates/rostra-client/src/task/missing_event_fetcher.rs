use std::sync::Arc;
use std::time::Duration;

use rostra_core::ShortEventId;
use rostra_core::event::{EventExt as _, SignedEventExt as _, VerifiedEvent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::{FmtCompact as _, WhateverResult};
use snafu::ResultExt as _;
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
            match wait_for_retry_or_notification(&mut notifications, retry.deadline).await {
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
                WakeReason::Lagging => {
                    warn!(target: LOG_TARGET, "Missing event fetcher missed notifications; reconciling durable work");
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

        for follower_id in followers.iter().chain([author_id, self.self_id].iter()) {
            debug!(
                target: LOG_TARGET,
                author_id = %author_id,
                follower_id = %follower_id,
                "Looking for missing events from peer"
            );
            let Ok(conn) = self
                .connections
                .get_or_connect(&self.networking, *follower_id)
                .await
            else {
                debug!(
                    target: LOG_TARGET,
                    author_id = %author_id,
                    follower_id = %follower_id,
                    "Could not connect"
                );
                continue;
            };

            for missing_event in &missing_events {
                if db.has_event(*missing_event).await {
                    continue;
                }
                let event = match self.get_event(author_id, *missing_event, &conn).await {
                    Ok(event) => event,
                    Err(err) => {
                        debug!(
                            target: LOG_TARGET,
                            author_id = %author_id,
                            event_id = %missing_event,
                            follower_id = %follower_id,
                            err = %err.fmt_compact(),
                            "Error getting event from a peer"
                        );
                        continue;
                    }
                };
                let Some(event) = event else {
                    continue;
                };
                if let Err(err) = db.try_process_event(&event).await {
                    error!(
                        target: LOG_TARGET,
                        author_id = %author_id,
                        event_id = %missing_event,
                        follower_id = %follower_id,
                        err = %err,
                        "Failed to store a fetched missing event"
                    );
                    return Err(err);
                }
            }
        }

        Ok(())
    }

    async fn get_event(
        &self,
        author_id: RostraId,
        event_id: ShortEventId,
        conn: &Connection,
    ) -> WhateverResult<Option<VerifiedEvent>> {
        let event = conn
            .get_event(author_id, event_id)
            .await
            .whatever_context("Failed to query peer")?;

        let Some(event) = event else {
            return Ok(None);
        };
        let event =
            VerifiedEvent::verify_response(author_id, event_id, *event.event(), event.sig())
                .whatever_context("Invalid event received")?;

        Ok(Some(event))
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
