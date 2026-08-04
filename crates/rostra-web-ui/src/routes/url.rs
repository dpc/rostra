use std::str::FromStr;

use axum::http::Uri;
use axum::response::{IntoResponse, Redirect, Response};
use rostra_client_db::Database;
use rostra_core::id::{RostraId, ShortRostraId, ToShort as _};
use rostra_core::{EventId, ShortEventId};
use serde::{Deserialize, Deserializer};

/// An identity path component in either legacy full or canonical short form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RostraPathId {
    /// A full identity accepted for compatibility.
    Full(RostraId),
    /// A canonical short identity requiring retained-index resolution.
    Short(ShortRostraId),
}

impl FromStr for RostraPathId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        RostraId::from_str(value)
            .map(Self::Full)
            .or_else(|_| ShortRostraId::from_str(value).map(Self::Short))
            .map_err(|error| error.to_string())
    }
}

impl<'de> Deserialize<'de> for RostraPathId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl RostraPathId {
    /// Resolve a path identity without allowing a short prefix to select
    /// arbitrarily.
    pub(crate) async fn resolve(self, db: &Database) -> Option<RostraId> {
        match self {
            Self::Full(id) => Some(id),
            Self::Short(id) => db.get_known_identity(id).await,
        }
    }
}

/// An event path component in either legacy full or canonical short form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventPathId {
    /// A full event ID accepted for compatibility.
    Full(EventId),
    /// A canonical short event ID.
    Short(ShortEventId),
}

impl FromStr for EventPathId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ShortEventId::from_str(value)
            .map(Self::Short)
            .or_else(|_| EventId::from_str(value).map(Self::Full))
            .map_err(|error| error.to_string())
    }
}

impl<'de> Deserialize<'de> for EventPathId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl EventPathId {
    /// Resolve an event path component against its retained envelope.
    ///
    /// A legacy full ID must exactly match the retained event before this
    /// method permits shortening it.
    pub(crate) async fn resolve(self, db: &Database) -> Option<ShortEventId> {
        match self {
            Self::Full(id) => {
                let short_id = id.to_short();
                let event = db.get_event(short_id).await?;
                (event.signed.compute_id() == id).then_some(short_id)
            }
            Self::Short(id) => Some(id),
        }
    }
}

/// Redirect a GET/HEAD request when its path differs from the canonical path.
pub(crate) fn redirect_to_canonical(
    original_uri: &Uri,
    canonical_path: String,
) -> Option<Response> {
    (original_uri.path() != canonical_path).then(|| {
        let location = original_uri
            .query()
            .map(|query| format!("{canonical_path}?{query}"))
            .unwrap_or(canonical_path);
        Redirect::permanent(&location).into_response()
    })
}

/// Return the canonical relative URL for a profile page.
pub(crate) fn profile_url(id: RostraId) -> String {
    format!("/profile/{}", id.to_short())
}

/// Return the canonical relative URL for a profile avatar.
pub(crate) fn avatar_url(id: RostraId, event_id: ShortEventId) -> String {
    format!("{}?v={event_id}", avatar_path(id))
}

/// Return the canonical relative path for a profile avatar.
pub(crate) fn avatar_path(id: RostraId) -> String {
    format!("{}/avatar", profile_url(id))
}

/// Return the canonical relative URL for an Atom profile feed.
pub(crate) fn profile_feed_url(id: RostraId) -> String {
    format!("{}/atom.xml", profile_url(id))
}

/// Return the canonical relative URL for a profile follow action.
pub(crate) fn profile_follow_url(id: RostraId) -> String {
    format!("{}/follow", profile_url(id))
}

/// Return the canonical relative URL for a media resource.
pub(crate) fn media_url(author: RostraId, event_id: ShortEventId) -> String {
    format!("/media/{}/{event_id}", author.to_short())
}

/// Return the canonical relative URL for an author's media list.
pub(crate) fn media_list_url(author: RostraId) -> String {
    format!("/media/{}/list", author.to_short())
}

/// Return the canonical relative URL for a post.
pub(crate) fn post_url(author: RostraId, event_id: ShortEventId) -> String {
    format!("/post/{}/{event_id}", author.to_short())
}

/// Return the canonical relative URL for editing a post.
pub(crate) fn post_edit_url(author: RostraId, event_id: ShortEventId) -> String {
    format!("{}/edit", post_url(author, event_id))
}

/// Return the canonical relative URL for deleting a post.
pub(crate) fn post_delete_url(author: RostraId, event_id: ShortEventId) -> String {
    format!("{}/delete", post_url(author, event_id))
}

/// Return the canonical relative URL for cancelling a post edit.
pub(crate) fn post_edit_cancel_url(author: RostraId, event_id: ShortEventId) -> String {
    format!("{}/edit_cancel", post_url(author, event_id))
}

/// Return the canonical relative URL for fetching missing post content.
pub(crate) fn post_fetch_url(
    post_thread_id: ShortEventId,
    author: RostraId,
    event_id: ShortEventId,
) -> String {
    format!(
        "/post/{post_thread_id}/{}/{event_id}/fetch",
        author.to_short()
    )
}

/// Return the canonical relative URL for a post's replies.
pub(crate) fn replies_url(post_thread_id: ShortEventId, event_id: ShortEventId) -> String {
    format!("/replies/{post_thread_id}/{event_id}")
}

/// Return the canonical relative URL for event content in settings.
pub(crate) fn settings_event_content_url(event_id: ShortEventId) -> String {
    format!("/settings/events/content/{event_id}")
}

#[cfg(test)]
mod tests;
