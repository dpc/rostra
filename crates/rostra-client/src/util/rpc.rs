use std::collections::{BTreeMap, BinaryHeap, HashSet};

use futures::stream::{self, StreamExt as _};
use rostra_client_db::{InsertEventOutcome, ProcessEventState};
use rostra_core::ShortEventId;
use rostra_core::event::{EventExt as _, VerifiedEvent};
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

/// Download events from a head, traversing the DAG using BinaryHeap.
///
/// This fetches events starting from `head` and exploring
/// parent_aux/parent_prev. Returns true if any new data was downloaded.
pub async fn download_events_from_head(
    rostra_id: RostraId,
    head: ShortEventId,
    conn: &Connection,
    storage: &rostra_client_db::Database,
) -> WhateverResult<bool> {
    let mut events = BinaryHeap::from([(0i32, head)]);
    let mut downloaded_anything = false;

    let peer_id = conn.remote_id();

    while let Some((depth, event_id)) = events.pop() {
        // Check if we already have this event locally
        let (event, _insert_outcome, process_state) =
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
                    InsertEventOutcome::AlreadyPresent,
                    ProcessEventState::Existing,
                )
            } else {
                debug!(
                   target: LOG_TARGET,
                    %depth,
                    node_id = %peer_id,
                    %rostra_id,
                    %event_id,
                    "Querying for event"
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
                        "Event not found"
                    );
                    continue;
                };
                downloaded_anything = true;
                let (insert_outcome, process_state) = storage.process_event(&new_event).await;

                (new_event, insert_outcome, process_state)
            };

        // Always add aux parent, as it allows us to explore the history fast and
        // somewhat randomly
        if let Some(parent) = event.parent_aux() {
            events.push((depth - 1, parent));
        }

        if storage.wants_content(event_id, process_state).await {
            // If we wanted content of the event, we might need content of the previous
            // closest parent as well
            if let Some(parent) = event.parent_prev() {
                events.push((depth - 1, parent));
            }
            let content = conn
                .get_event_content(event)
                .await
                .whatever_context("Failed to download peer data")?;

            if let Some(content) = content {
                storage.process_event_content(&content).await;
            } else {
                debug!(
                    target: LOG_TARGET,
                    %depth,
                    node_id = %peer_id,
                    %rostra_id,
                    %event_id,
                    "Event content not found"
                );
            }
        } else {
            debug!(
                target: LOG_TARGET,
                %rostra_id,
                %event_id,
                "Event content not wanted"
            );
        }
    }

    Ok(downloaded_anything)
}
