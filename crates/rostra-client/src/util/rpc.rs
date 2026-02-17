use std::collections::{BTreeMap, HashSet};

use futures::stream::{self, StreamExt as _};
use rostra_client_db::{InsertEventOutcome, ProcessEventState};
use rostra_core::ShortEventId;
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::{BoxedErrorResult, FmtCompact as _, WhateverResult};
use snafu::{OptionExt as _, ResultExt as _, Snafu};
use tracing::debug;

#[derive(Debug, Snafu)]
#[snafu(display("Event content not found from any peer"))]
pub struct ContentNotFoundError;

use crate::LOG_TARGET;
use crate::connection_cache::ConnectionCache;

pub async fn get_event_content_from_followers(
    client: crate::client::ClientHandle,
    self_id: RostraId,
    author_id: RostraId,
    event_id: ShortEventId,
    connections_cache: &ConnectionCache,
    followers_by_followee_cache: &mut BTreeMap<RostraId, Vec<RostraId>>,
    db: &rostra_client_db::Database,
) -> BoxedErrorResult<()> {
    let followers = if let Some(followers) = followers_by_followee_cache.get(&author_id) {
        followers
    } else {
        let followers = db.get_followers(author_id).await;
        followers_by_followee_cache.insert(author_id, followers);

        followers_by_followee_cache
            .get(&author_id)
            .expect("Just inserted")
    };

    // Create a stream of all potential sources (followers + author + self)
    let all_peers: HashSet<RostraId> = followers
        .iter()
        .cloned()
        .chain([author_id, self_id])
        .collect();

    debug!(target: LOG_TARGET,
        author_id = %author_id.to_short(),
        event_id = %event_id.to_short(),
        num_peers = all_peers.len(),
        "Attempting to fetch event content from peers"
    );

    let found = futures_lite::StreamExt::find_map(
        &mut stream::iter(all_peers)
            .map(|follower_id| {
                let client = client.clone();
                let connections_cache = connections_cache.clone();
                async move {
                    let Ok(client_ref) = client.client_ref().boxed() else {
                        return None;
                    };

                    let conn = match connections_cache
                        .get_or_connect(&client_ref, follower_id)
                        .await
                    {
                        Ok(conn) => conn,
                        Err(err) => {
                            debug!(target: LOG_TARGET,
                                author_id = %author_id.to_short(),
                                event_id = %event_id.to_short(),
                                peer_id = %follower_id.to_short(),
                                err = %err.fmt_compact(),
                                "Failed to connect to peer"
                            );
                            return None;
                        }
                    };

                    debug!(target: LOG_TARGET,
                        author_id = %author_id.to_short(),
                        event_id = %event_id.to_short(),
                        peer_id = %follower_id.to_short(),
                        "Getting event content from a peer"
                    );

                    match fetch_event_content_only(event_id, &conn, db).await {
                        Ok(true) => Some(()),
                        Ok(false) => {
                            debug!(target: LOG_TARGET,
                                author_id = %author_id.to_short(),
                                event_id = %event_id.to_short(),
                                peer_id = %follower_id.to_short(),
                                "Peer does not have the content"
                            );
                            None
                        }
                        Err(err) => {
                            debug!(target: LOG_TARGET,
                                author_id = %author_id.to_short(),
                                event_id = %event_id.to_short(),
                                peer_id = %follower_id.to_short(),
                                err = %err.fmt_compact(),
                                "Error getting event content from peer"
                            );
                            None
                        }
                    }
                }
            })
            .buffer_unordered(10),
        |result| result,
    )
    .await;

    if found.is_none() {
        debug!(target: LOG_TARGET,
            author_id = %author_id.to_short(),
            event_id = %event_id.to_short(),
            "No peer had the event content"
        );
        return Err(ContentNotFoundError.into());
    }

    Ok(())
}

/// Fetches only the content for an event that already exists in storage.
///
/// This is the specialized version used in missing_event_content_fetcher.rs.
pub async fn fetch_event_content_only(
    event_id: ShortEventId,
    conn: &Connection,
    storage: &rostra_client_db::Database,
) -> WhateverResult<bool> {
    let event = storage
        .get_event(event_id)
        .await
        .whatever_context("Unknown event")?;

    let event = VerifiedEvent::assume_verified_from_signed(event.signed);
    let content = conn
        .get_event_content(event)
        .await
        .whatever_context("Failed to download event content")?;

    if let Some(content) = content {
        storage.process_event_content(&content).await;
        return Ok(true);
    }

    Ok(false)
}

/// Download events from a child event, traversing the DAG depth-first by
/// timestamp (kinda).
///
/// Uses a BTreeMap-based queue sorted by (timestamp, depth, event_id) to
/// process events from oldest to newest. This ensures we fetch parent events
/// before their children, and we can establish a "cutoff" timestamp beyond
/// which we don't need to fetch more events (because we already have processed
/// content for those).
///
/// Returns true if any new data was downloaded.
pub async fn download_events_from_child(
    rostra_id: RostraId,
    head: ShortEventId,
    conn: &Connection,
    storage: &rostra_client_db::Database,
) -> WhateverResult<bool> {
    use rostra_core::event::EventExt as _;

    // Queue: (child_timestamp, depth, event_id) -> Option<ProcessEventState>
    // None means we haven't fetched/checked this event yet
    // Some means we have the event and know its ProcessEventState
    let mut queue: BTreeMap<(u64, u64, ShortEventId), Option<ProcessEventState>> = BTreeMap::new();

    // Cutoff timestamp: events with timestamps <= this are considered past
    // the point where we must have processed this part of the history in the past.
    let mut cutoff_timestamp: u64 = 0;

    let mut downloaded_anything = false;
    let peer_id = conn.remote_id();

    // Start with head at "far future" timestamp and max depth
    let far_future_ts = u64::MAX;
    let max_depth = u64::MAX;
    queue.insert((far_future_ts, max_depth, head), None);

    while let Some((&(child_timestamp, depth, event_id), &process_state_opt)) =
        queue.first_key_value()
    {
        // If we already have a ProcessEventState, it means we proccessed
        // this node and its parents before, and we can download the content if needed
        if let Some(process_state) = process_state_opt {
            queue.remove(&(child_timestamp, depth, event_id));

            if storage.wants_content(event_id, process_state).await {
                debug!(
                    target: LOG_TARGET,
                    %depth,
                    %rostra_id,
                    %event_id,
                    "Downloading content for event"
                );
                if let Some(event_record) = storage.get_event(event_id).await {
                    let event = VerifiedEvent::assume_verified_from_signed(event_record.signed);
                    let content = conn
                        .get_event_content(event)
                        .await
                        .whatever_context("Failed to download event content")?;

                    if let Some(content) = content {
                        storage.process_event_content(&content).await;
                    } else {
                        debug!(
                            target: LOG_TARGET,
                            %depth,
                            node_id = %peer_id,
                            %rostra_id,
                            %event_id,
                            "Event content not found on peer"
                        );
                    }
                }
            }
            continue;
        }

        // Fetch the event if we don't have it
        let (event, process_state, insert_outcome) =
            if let Some(local_event) = storage.get_event(event_id).await {
                debug!(
                    target: LOG_TARGET,
                    %depth,
                    %rostra_id,
                    %event_id,
                    "Event already exists locally"
                );
                let event = VerifiedEvent::assume_verified_from_signed(local_event.signed);
                (
                    event,
                    ProcessEventState::Existing,
                    InsertEventOutcome::AlreadyPresent,
                )
            } else {
                debug!(
                    target: LOG_TARGET,
                    %depth,
                    node_id = %peer_id,
                    %rostra_id,
                    %event_id,
                    "Querying peer for event"
                );
                let Some(new_event) = conn
                    .get_event(rostra_id, event_id)
                    .await
                    .whatever_context("Failed to query peer")?
                else {
                    debug!(
                        target: LOG_TARGET,
                        %depth,
                        node_id = %peer_id,
                        %rostra_id,
                        %event_id,
                        "Event not found on peer"
                    );
                    queue.remove(&(child_timestamp, depth, event_id));
                    continue;
                };
                downloaded_anything = true;
                let (insert_outcome, process_state) = storage.process_event(&new_event).await;
                (new_event, process_state, insert_outcome)
            };

        // Check if this event is past the cutoff, so we can skip
        // doing the heavier checks over and over.
        if event.timestamp().as_u64() < cutoff_timestamp {
            queue.remove(&(child_timestamp, depth, event_id));
            debug!(
                target: LOG_TARGET,
                %depth,
                %child_timestamp,
                %cutoff_timestamp,
                %rostra_id,
                %event_id,
                "Event past cutoff, skipping"
            );
            continue;
        }

        // Update queue entry with the process state
        queue.insert((child_timestamp, depth, event_id), Some(process_state));

        // Check if content is missing (we need to fetch it)
        let content_is_missing = storage.is_event_content_missing(event_id).await;

        if !content_is_missing && matches!(insert_outcome, InsertEventOutcome::AlreadyPresent) {
            // Content is not missing, and event isn't new - we have it and processed it
            // before. Update cutoff and remove from queue.
            queue.remove(&(child_timestamp, depth, event_id));
            cutoff_timestamp = cutoff_timestamp.max(child_timestamp);
            debug!(
                target: LOG_TARGET,
                %depth,
                %rostra_id,
                %event_id,
                new_cutoff = %cutoff_timestamp,
                "Content already present, updating cutoff"
            );
            continue;
        }

        // Content is missing - add parents to queue
        // Protect against parents with ts lower their ancestors.
        let parent_child_timestamp = event.event.timestamp.as_u64().min(child_timestamp);
        let parent_depth = depth.saturating_sub(1);

        for parent_id in [event.parent_prev(), event.parent_aux()]
            .into_iter()
            .flatten()
        {
            queue.insert((parent_child_timestamp, parent_depth, parent_id), None);

            debug!(
                target: LOG_TARGET,
                %rostra_id,
                %event_id,
                %parent_id,
                %parent_child_timestamp,
                %parent_depth,
                "Added parent to queue"
            );
        }
    }

    Ok(downloaded_anything)
}
