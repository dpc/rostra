use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use rostra_client_db::{
    CurrentState, Database, EventContentState, EventRecord, IdsFollowersRecord,
};
use rostra_core::ShortEventId;
use rostra_core::event::{EventContentRaw, EventExt as _, SignedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::{FmtCompact, WhateverResult};
use snafu::ResultExt as _;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tracing::{debug, instrument, trace, warn};

/// Arc-wrapped followers map for cheap cloning
type FollowersMap = Arc<HashMap<RostraId, IdsFollowersRecord>>;

use crate::client::Client;
use crate::task::head_selection::sorted_heads;
use crate::task::outbound_deadline::{PEER_OPERATION_DEADLINE, within};

const LOG_TARGET: &str = "rostra::head_broadcaster";
const BROADCAST_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const BROADCAST_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct BroadcastPolicy {
    peer_deadline: Duration,
    retry_initial_delay: Duration,
    retry_max_delay: Duration,
}

const BROADCAST_POLICY: BroadcastPolicy = BroadcastPolicy {
    peer_deadline: PEER_OPERATION_DEADLINE,
    retry_initial_delay: BROADCAST_RETRY_INITIAL_DELAY,
    retry_max_delay: BROADCAST_RETRY_MAX_DELAY,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BroadcastHeadOutcome {
    Delivered,
    Retry,
    Stop,
}

pub struct HeadUpdateBroadcaster {
    client: crate::client::ClientHandle,
    networking: Arc<crate::net::ClientNetworking>,
    db: Arc<Database>,
    self_id: RostraId,
    self_followers: CurrentState<FollowersMap>,
    new_heads_rx: broadcast::Receiver<(RostraId, ShortEventId)>,
    new_content_rx: broadcast::Receiver<VerifiedEventContent>,
}

impl HeadUpdateBroadcaster {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head broadcasting task" );
        Self {
            client: client.handle(),
            networking: client.networking().clone(),
            db: client.db().to_owned(),
            self_id: client.rostra_id(),

            self_followers: client.self_followers_subscribe(),
            new_heads_rx: client.db().new_heads_subscribe(),
            new_content_rx: client.db().new_content_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(name = "head-update-broadcaster", skip(self), fields(self_id = %self.self_id.fmt_short()), ret)]
    pub async fn run(self) {
        self.run_with_policy(BROADCAST_POLICY).await;
    }

    async fn run_with_policy(mut self, policy: BroadcastPolicy) {
        let mut self_followers = self.self_followers.clone();
        let mut pending_heads = BTreeSet::new();
        let mut retry_at = BTreeMap::new();
        reconcile_pending_heads(&self.db, &mut pending_heads, &mut retry_at).await;

        loop {
            let followers = self_followers.snapshot();
            if let Some((head, event, event_content)) =
                take_one_ready_head(&self.db, &mut pending_heads, &retry_at).await
            {
                match self
                    .broadcast_head(
                        head,
                        &event,
                        &event_content,
                        &followers,
                        policy.peer_deadline,
                    )
                    .await
                {
                    BroadcastHeadOutcome::Delivered => {
                        pending_heads.remove(&head);
                        retry_at.remove(&head);
                    }
                    BroadcastHeadOutcome::Retry => {
                        let retry_count = retry_at
                            .get(&head)
                            .map_or(0, |retry_at| retry_at.retry_count);
                        retry_at.insert(
                            head,
                            BroadcastRetry {
                                retry_count: retry_count.saturating_add(1),
                                at: Instant::now() + broadcast_retry_delay(retry_count, policy),
                            },
                        );
                    }
                    BroadcastHeadOutcome::Stop => return,
                }
                continue;
            }

            loop {
                let next_retry = retry_at
                    .iter()
                    .filter(|(head, _)| pending_heads.contains(head))
                    .map(|(_, retry_at)| retry_at.at)
                    .min()
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(24 * 60 * 60));
                let should_retry = tokio::select! {
                    res = self.new_heads_rx.recv() => {
                        match res {
                        Ok((author, head)) if author == self.self_id => {
                            trace!(target: LOG_TARGET, event_id = %head.to_short(), "Received exact self-head signal");
                            reconcile_pending_heads(&self.db, &mut pending_heads, &mut retry_at).await;
                            true
                        }
                        Ok(_) => false,
                        Err(broadcast::error::RecvError::Closed) => return,
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(
                                target: LOG_TARGET,
                                skipped,
                                "Head broadcast receiver lagged; recovering durable heads"
                            );
                            reconcile_pending_heads(&self.db, &mut pending_heads, &mut retry_at).await;
                            true
                        }
                    }
                    }
                    res = self.new_content_rx.recv() => {
                        match res {
                        Ok(content)
                            if content_completes_pending(
                                &content,
                                self.self_id,
                                &pending_heads,
                            ) =>
                        {
                            true
                        }
                        Ok(_) => false,
                        Err(broadcast::error::RecvError::Closed) => return,
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(
                                target: LOG_TARGET,
                                skipped,
                                "Content broadcast receiver lagged; reconciling durable heads"
                            );
                            reconcile_pending_heads(&self.db, &mut pending_heads, &mut retry_at).await;
                            true
                        }
                    }
                    }
                    res = self_followers.changed() => {
                        if res.is_err() {
                            return;
                        }
                        // New followers did not observe earlier incremental signals.
                        // Recover from the complete durable set.
                        reconcile_pending_heads(&self.db, &mut pending_heads, &mut retry_at).await;
                        true
                    }
                    _ = tokio::time::sleep_until(next_retry) => true,
                };
                if should_retry {
                    break;
                }
            }
            trace!(target: LOG_TARGET, "Woke up");
        }
    }

    async fn broadcast_head(
        &self,
        head: ShortEventId,
        event: &EventRecord,
        event_content: &EventContentRaw,
        followers: &FollowersMap,
        deadline: Duration,
    ) -> BroadcastHeadOutcome {
        debug!(
            target: LOG_TARGET,
            event_id = %head.to_short(),
            followers_num = followers.len(),
            "Broadcasting new head event to followers"
        );

        let mut retry = false;

        // Send to ourselves first, in case we have redundant nodes.
        for id in [self.self_id].into_iter().chain(followers.keys().copied()) {
            if self.client.app_ref_opt().is_none() {
                debug!(target: LOG_TARGET, "Client gone, quitting");
                return BroadcastHeadOutcome::Stop;
            }

            match within(
                deadline,
                self.broadcast_event(id, &event.signed, event_content),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    retry = true;
                    debug!(
                        target: LOG_TARGET,
                        err = %err.fmt_compact(),
                        id = %id.to_short(),
                        "Failed to broadcast new head to node"
                    );
                }
                Err(_) => {
                    retry = true;
                    warn!(
                        target: LOG_TARGET,
                        id = %id.to_short(),
                        timeout_ms = deadline.as_millis(),
                        "Timed out broadcasting new head to node"
                    );
                }
            }
        }
        if retry {
            BroadcastHeadOutcome::Retry
        } else {
            BroadcastHeadOutcome::Delivered
        }
    }

    async fn broadcast_event(
        &self,
        id: RostraId,
        signed_event: &SignedEvent,
        event_content: &EventContentRaw,
    ) -> WhateverResult<()> {
        let conn = self
            .networking
            .connect_cached(id)
            .await
            .whatever_context("Couldn't connect")?;

        conn.feed_event(*signed_event, event_content.clone())
            .await
            .whatever_context("Failed broadcasting head event")?;

        Ok(())
    }
}

async fn reconcile_current_heads(db: &Database, pending_heads: &mut BTreeSet<ShortEventId>) {
    *pending_heads = sorted_heads(&db.get_heads_self().await)
        .into_iter()
        .collect();
}

async fn reconcile_pending_heads(
    db: &Database,
    pending_heads: &mut BTreeSet<ShortEventId>,
    retry_at: &mut BTreeMap<ShortEventId, BroadcastRetry>,
) {
    reconcile_current_heads(db, pending_heads).await;
    retry_at.retain(|head, _| pending_heads.contains(head));
}

#[derive(Debug, Clone, Copy)]
struct BroadcastRetry {
    retry_count: u32,
    at: Instant,
}

fn broadcast_retry_delay(retry_count: u32, policy: BroadcastPolicy) -> Duration {
    policy
        .retry_initial_delay
        .checked_mul(1u32 << retry_count.min(31))
        .unwrap_or(Duration::MAX)
        .min(policy.retry_max_delay)
}

fn content_completes_pending(
    content: &VerifiedEventContent,
    self_id: RostraId,
    pending_heads: &BTreeSet<ShortEventId>,
) -> bool {
    content.event.event.author == self_id && pending_heads.contains(&content.event_id().to_short())
}

async fn take_one_ready_head(
    db: &Database,
    pending_heads: &mut BTreeSet<ShortEventId>,
    retry_at: &BTreeMap<ShortEventId, BroadcastRetry>,
) -> Option<(ShortEventId, EventRecord, EventContentRaw)> {
    for head in pending_heads.iter().copied().collect::<Vec<_>>() {
        if retry_at
            .get(&head)
            .is_some_and(|retry_at| Instant::now() < retry_at.at)
        {
            continue;
        }
        let Some(event) = db.get_event(head).await else {
            warn!(target: LOG_TARGET, event_id = %head.to_short(), "No head event!?");
            pending_heads.remove(&head);
            continue;
        };
        let content = db.get_event_content(head).await;
        if let Some(content) = content {
            return Some((head, event, content));
        }
        if event.content_len() == 0 {
            return Some((head, event, EventContentRaw::new(Vec::new())));
        }

        let content_state = db.get_event_content_state(head).await;
        if content_is_terminal(content_state) {
            debug!(target: LOG_TARGET, event_id = %head.to_short(), "Head content is terminally unavailable");
            pending_heads.remove(&head);
        } else {
            // `None` may mean content became ready after the availability read.
            // Keep the head so the already-queued content signal retries it.
            trace!(target: LOG_TARGET, event_id = %head.to_short(), "Head content not ready");
        }
        continue;
    }
    None
}

fn content_is_terminal(content_state: Option<EventContentState>) -> bool {
    matches!(
        content_state,
        Some(
            EventContentState::Deleted { .. }
                | EventContentState::Pruned
                | EventContentState::Invalid
        )
    )
}

#[cfg(test)]
mod tests;
