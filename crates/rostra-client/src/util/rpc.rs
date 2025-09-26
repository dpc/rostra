use std::collections::BTreeMap;

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
    connections_cache: &mut ConnectionCache,
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

    for follower_id in followers.iter().chain([author_id, self_id].iter()) {
        let Ok(client) = client.client_ref().boxed() else {
            break;
        };
        let Some(conn) = connections_cache
            .get_or_connect(&client, *follower_id)
            .await
        else {
            continue;
        };

        debug!(target:  LOG_TARGET,
            author_id = %author_id.to_short(),
            event_id = %event_id.to_short(),
            follower_id = %follower_id.to_short(),
            "Getting event content from a peer"
        );
        match fetch_event_content_only(event_id, conn, db).await {
            Ok(true) => {
                return Ok(());
            }
            Ok(false) => {}
            Err(err) => {
                debug!(target:  LOG_TARGET,
                    author_id = %author_id.to_short(),
                    event_id = %event_id.to_short(),
                    follower_id = %follower_id.to_short(),
                    err = %err.fmt_compact(),
                    "Error getting event from a peer"
                );
            }
        }
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
