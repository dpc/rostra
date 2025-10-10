use std::collections::{BTreeMap, HashSet};

use futures::stream::{self, StreamExt as _};
use rostra_core::ShortEventId;
use rostra_core::event::VerifiedEvent;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_p2p::Connection;
use rostra_util_error::{BoxedErrorResult, FmtCompact as _, WhateverResult};
use snafu::{OptionExt as _, ResultExt as _};
use tracing::debug;

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

    let _result = futures_lite::StreamExt::find_map(
        &mut stream::iter(all_peers)
            .map(|follower_id| {
                let client = client.clone();
                let connections_cache = connections_cache.clone();
                async move {
                    let Ok(client_ref) = client.client_ref().boxed() else {
                        return None;
                    };

                    let conn = (connections_cache
                        .get_or_connect(&client_ref, follower_id)
                        .await)?;

                    debug!(target: LOG_TARGET,
                        author_id = %author_id.to_short(),
                        event_id = %event_id.to_short(),
                        follower_id = %follower_id.to_short(),
                        "Getting event content from a peer"
                    );

                    match fetch_event_content_only(event_id, &conn, db).await {
                        Ok(true) => Some(()),
                        Ok(false) => None,
                        Err(err) => {
                            debug!(target: LOG_TARGET,
                                author_id = %author_id.to_short(),
                                event_id = %event_id.to_short(),
                                follower_id = %follower_id.to_short(),
                                err = %err.fmt_compact(),
                                "Error getting event from a peer"
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
