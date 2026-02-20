use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use rostra_client_db::{InsertEventOutcome, ProcessEventState};
use rostra_core::ShortEventId;
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_util_error::{BoxedErrorResult, WhateverResult};
use rostra_util_fmt::AsFmtOption as _;
use snafu::{OptionExt as _, Snafu};
use tracing::debug;

#[derive(Debug, Snafu)]
#[snafu(display("Event content not found from any peer"))]
pub struct ContentNotFoundError;

use crate::LOG_TARGET;
use crate::connection_cache::ConnectionCache;
use crate::net::ClientNetworking;

pub async fn get_event_content_from_followers(
    networking: &ClientNetworking,
    self_id: RostraId,
    author_id: RostraId,
    event_id: ShortEventId,
    connections_cache: &ConnectionCache,
    followers_by_followee_cache: &mut BTreeMap<RostraId, Vec<RostraId>>,
    db: &rostra_client_db::Database,
) -> BoxedErrorResult<()> {
    let followers = if let Some(followers) = followers_by_followee_cache.get(&author_id) {
        followers.clone()
    } else {
        let followers = db.get_followers(author_id).await;
        followers_by_followee_cache.insert(author_id, followers.clone());
        followers
    };

    let peers: Vec<RostraId> = followers.into_iter().chain([author_id, self_id]).collect();

    debug!(
        target: LOG_TARGET,
        author = %author_id.to_short(),
        %event_id,
        peer_count = peers.len(),
        "Fetching event content from followers"
    );

    let event = if let Some(event_record) = db.get_event(event_id).await {
        VerifiedEvent::assume_verified_from_signed(event_record.signed)
    } else {
        debug!(
            target: LOG_TARGET,
            author = %author_id.to_short(),
            %event_id,
            "Event not in DB, fetching from peers first"
        );
        let event = connections_cache
            .get_event_from_peers(networking, &peers, author_id, event_id)
            .await
            .context(ContentNotFoundSnafu)?;
        db.process_event(&event).await;
        event
    };

    let content = connections_cache
        .get_event_content_from_peers(networking, &peers, event)
        .await
        .context(ContentNotFoundSnafu)?;

    db.process_event_content(&content).await;

    Ok(())
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
/// Depth starts at 0 for the head and increments for each parent traversal.
/// Higher depth (deeper into the DAG) is processed first via `Reverse`.
///
/// Tries multiple peers (via the connection cache) when fetching events and
/// content.
///
/// Returns true if any new data was downloaded.
pub async fn download_events_from_child(
    rostra_id: RostraId,
    head: ShortEventId,
    networking: &ClientNetworking,
    connections: &ConnectionCache,
    peers: &[RostraId],
    storage: &rostra_client_db::Database,
) -> WhateverResult<bool> {
    use rostra_core::event::EventExt as _;

    struct QueueItemData {
        process_state: Option<ProcessEventState>,
        child_timestamp: u64,
        /// Depth in the DAG from the starting head (0 = head, 1 = parent, etc.)
        depth: u64,
        event: Option<VerifiedEvent>,
    }
    debug!(
        target: LOG_TARGET,
        author = %rostra_id.to_short(),
        %head,
        "Fetching new events from new head/child"
    );

    // Queue key: (timestamp, Reverse(depth), event_id)
    // None means we haven't fetched/checked this event yet
    // Some means we have the event and know its ProcessEventState
    // Reverse(depth) ensures deeper events (higher depth) are processed first
    type QueueKey = (Option<(u64, Reverse<u64>)>, ShortEventId);
    let mut queue: BTreeSet<QueueKey> = BTreeSet::new();

    // Information about elements in the queue
    let mut in_queue: BTreeMap<ShortEventId, QueueItemData> = BTreeMap::new();

    let mut downloaded_anything = false;

    // Stats
    let mut max_queue_len: usize = 0;
    let mut events_traversed: usize = 0;
    let mut event_fetch_attempts: usize = 0;
    let mut content_fetch_attempts: usize = 0;
    let mut new_events: usize = 0;
    let mut new_contents: usize = 0;

    // Start with head at "far future" timestamp and depth 0
    let far_future_ts = u64::MAX;

    queue.insert((None, head));
    in_queue.insert(
        head,
        QueueItemData {
            process_state: None,
            child_timestamp: far_future_ts,
            depth: 0,
            event: None,
        },
    );

    while let Some((q_item_ts_and_depth, q_item_event_id)) = queue.pop_first() {
        // +1 because we just popped one
        max_queue_len = max_queue_len.max(queue.len() + 1);

        let q_item_data = in_queue
            .remove(&q_item_event_id)
            .expect("Must always be there");

        let q_item_depth = q_item_data.depth;
        // If we already have a ProcessEventState, it means we processed
        // this event and its parents already, and we can now download and process the
        // content if needed
        if let Some(process_state) = q_item_data.process_state {
            if storage.wants_content(q_item_event_id, process_state).await {
                let event = q_item_data
                    .event
                    .expect("Must have event set at this point");

                debug!(
                    target: LOG_TARGET,
                    depth = %q_item_depth,
                    event_id = %q_item_event_id,
                    "Downloading content for event"
                );

                content_fetch_attempts += 1;
                if let Some(content) = connections
                    .get_event_content_from_peers(networking, peers, event)
                    .await
                {
                    storage.process_event_content(&content).await;
                    new_contents += 1;
                }
            }
            continue;
        }

        // Every event goes through here once on the first time
        events_traversed += 1;

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
                    event_id = %q_item_event_id,
                    "Querying peers for event"
                );

                event_fetch_attempts += 1;
                let Some(new_event) = connections
                    .get_event_from_peers(networking, peers, rostra_id, q_item_event_id)
                    .await
                else {
                    debug!(
                        target: LOG_TARGET,
                        depth = %q_item_depth,
                        event_id = %q_item_event_id,
                        "Failed to fetch event from any peer, skipping"
                    );
                    continue;
                };
                downloaded_anything = true;
                new_events += 1;
                let (insert_outcome, process_state) = storage.process_event(&new_event).await;
                (new_event, process_state, insert_outcome)
            };

        let q_item_ts = q_item_data.child_timestamp.min(event.timestamp().as_u64());

        queue.insert((Some((q_item_ts, Reverse(q_item_depth))), q_item_event_id));
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
            && !rand::random::<u8>().is_multiple_of(4)
        {
            continue;
        }

        // Add parents to be processed
        let parents: Vec<_> = [event.parent_prev(), event.parent_aux()]
            .into_iter()
            .flatten()
            .collect();

        let parent_prev = event.parent_prev();
        let parent_aux = event.parent_aux();

        debug!(
            target: LOG_TARGET,
            event_id = %q_item_event_id,
            parent_prev = %parent_prev.fmt_option(),
            parent_aux = %parent_aux.fmt_option(),
            parent_count = parents.len(),
            "Queueing parents for event"
        );

        let parent_depth = q_item_depth + 1;

        for parent_id in parents {
            if let Some(existing) = in_queue.get_mut(&parent_id) {
                existing.child_timestamp = existing.child_timestamp.min(q_item_ts);
                existing.depth = existing.depth.max(parent_depth);
                continue;
            }
            queue.insert((None, parent_id));
            in_queue.insert(
                parent_id,
                QueueItemData {
                    process_state: None,
                    child_timestamp: q_item_ts,
                    depth: parent_depth,
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

    debug!(
        target: LOG_TARGET,
        author = %rostra_id.to_short(),
        %head,
        max_queue_len,
        events_traversed,
        event_fetch_attempts,
        content_fetch_attempts,
        new_events,
        new_contents,
        "Finished downloading events from head"
    );

    Ok(downloaded_anything)
}
