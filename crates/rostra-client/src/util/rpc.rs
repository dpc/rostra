use std::collections::{BTreeMap, BTreeSet, HashSet};

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
    // TODO: this should be connection pool so we can connect to any source of events we can
    conn: &Connection,
    storage: &rostra_client_db::Database,
) -> WhateverResult<bool> {
    use rostra_core::event::EventExt as _;

    struct QueueItemData {
        process_state: Option<ProcessEventState>,
        child_timestamp: u64,
        child_depth: u64,
        event: Option<VerifiedEvent>,
    }

    // Queue: (timestamp, depth, event_id) -> Option<ProcessEventState>
    // None means we haven't fetched/checked this event yet
    // Some means we have the event and know its ProcessEventState
    let mut queue: BTreeSet<(Option<(u64, u64)>, ShortEventId)> = BTreeSet::new();

    // Information about elements in the queue
    let mut in_queue: BTreeMap<ShortEventId, QueueItemData> = BTreeMap::new();

    let mut downloaded_anything = false;
    let peer_id = conn.remote_id();

    // Start with head at "far future" timestamp and max depth
    let far_future_ts = u64::MAX;
    let max_depth = u64::MAX;

    queue.insert((None, head));
    in_queue.insert(
        head,
        QueueItemData {
            process_state: None,
            child_timestamp: far_future_ts,
            child_depth: max_depth,
            event: None,
        },
    );

    while let Some((q_item_ts_and_depth, q_item_event_id)) = queue.pop_first() {
        let q_item_data = in_queue
            .remove(&q_item_event_id)
            .expect("Must always be there");

        let q_item_depth = q_item_data.child_depth - 1;
        // If we already have a ProcessEventState, it means we processed
        // this event and its parents already, and we can now download and process the
        // content if needed
        if let Some(process_state) = q_item_data.process_state {
            if storage.wants_content(q_item_event_id, process_state).await {
                debug!(
                    target: LOG_TARGET,
                            depth = %q_item_depth,
                            node_id = %peer_id,
                            event_id = %q_item_event_id,
                    "Downloading content for event"
                );
                // TODO: fetch the content from author, our own ids, and followers
                let content = match conn
                    .get_event_content(
                        q_item_data
                            .event
                            .expect("Must have event set at this point"),
                    )
                    .await
                {
                    Ok(c) => c,
                    Err(err) => {
                        debug!(
                            target: LOG_TARGET,
                            depth = %q_item_depth,
                            node_id = %peer_id,
                            event_id = %q_item_event_id,
                            "Event content not found"
                        );
                        // Note: we are not inserting the event back to the queue,
                        // effectively skipping processing it, as we were not able
                        // to get it at all.
                        continue;
                    }
                };

                if let Some(content) = content {
                    storage.process_event_content(&content).await;
                } else {
                    debug!(
                        target: LOG_TARGET,
                        depth = %q_item_depth,
                        node_id = %peer_id,
                        event_id = %q_item_event_id,
                        "Event content not found"
                    );
                    continue;
                }
            }
            continue;
        }

        assert!(q_item_ts_and_depth.is_none());
        // Fetch the event if we don't have it
        let (event, process_state, insert_outcome) =
            if let Some(local_event) = storage.get_event(q_item_event_id).await {
                debug!(
                    target: LOG_TARGET,
                    depth = %q_item_depth,
                    event_id = %q_item_event_id,
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
                    depth = %q_item_depth,
                    node_id = %peer_id,
                    event_id = %q_item_event_id,
                    "Querying peer for event"
                );
                // TODO: we must try to get the event from themselves, our own other ids or
                // their followers instead of just them
                let new_event_res = match conn.get_event(rostra_id, q_item_event_id).await {
                    Ok(e) => e,
                    Err(err) => {
                        debug!(
                            target: LOG_TARGET,
                            depth = %q_item_depth,
                            node_id = %peer_id,
                            event_id = %q_item_event_id,
                            "Failed to fetch event"
                        );
                        // Note: we are not inserting the event back to the queue,
                        // effectively skipping processing it, as we were not able
                        // to get it at all.
                        continue;
                    }
                };
                let Some(new_event) = new_event_res else {
                    debug!(
                        target: LOG_TARGET,
                        depth = %q_item_depth,
                        node_id = %peer_id,
                        event_id = %q_item_event_id,
                        "Event not found on peer"
                    );
                    // Note: we are not inserting the event back to the queue,
                    // effectively skipping processing it, as we were not able
                    // to get it at all.
                    continue;
                };
                downloaded_anything = true;
                let (insert_outcome, process_state) = storage.process_event(&new_event).await;
                (new_event, process_state, insert_outcome)
            };

        let q_item_ts_and_depth = (
            q_item_data.child_timestamp.min(event.timestamp().as_u64()),
            q_item_depth,
        );

        queue.insert((Some(q_item_ts_and_depth), q_item_event_id));
        in_queue.insert(
            q_item_event_id,
            QueueItemData {
                process_state: Some(process_state),
                event: Some(event),
                ..q_item_data
            },
        );

        if !storage.wants_content(q_item_event_id, process_state).await
                && matches!(insert_outcome, InsertEventOutcome::AlreadyPresent)
                // TODO: or with probability of 50%, instead of false here
                && false
        {
            // Since we don't want
            continue;
        }

        // Add parents to be processed
        for parent_id in [event.parent_prev(), event.parent_aux()]
            .into_iter()
            .flatten()
        {
            if in_queue.contains_key(&parent_id) {
                // TODO: update data for the parent_id inside in_queue to be minimum
                // child_timestamp and child_depth of this iteration and what's
                // already there
                continue;
            }
            queue.insert((None, parent_id));
            in_queue.insert(
                parent_id,
                QueueItemData {
                    process_state: None,
                    child_timestamp: q_item_ts_and_depth.0,
                    child_depth: q_item_ts_and_depth.1,
                    event: None,
                },
            );

            debug!(
                target: LOG_TARGET,
                %rostra_id,
                child_event_id = %q_item_event_id,
                %parent_id,
                "Added parent to queue"
            );
        }
    }

    Ok(downloaded_anything)
}
