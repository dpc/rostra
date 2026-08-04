use atom_syndication::{Content, Entry, Feed, Link, Person};
use axum::extract::{OriginalUri, Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};

use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;
use crate::routes::url::{RostraPathId, post_url, profile_feed_url, redirect_to_canonical};

/// Response wrapper for Atom feeds
pub struct AtomFeed(pub Feed);

impl IntoResponse for AtomFeed {
    fn into_response(self) -> Response {
        (
            [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
            self.0.to_string(),
        )
            .into_response()
    }
}

pub async fn get_profile_feed_atom(
    state: State<SharedState>,
    session: UserSession,
    OriginalUri(original_uri): OriginalUri,
    Path(profile_id): Path<RostraPathId>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let Some(profile_id) = profile_id.resolve(client_ref.db()).await else {
        return Ok(axum::http::StatusCode::NOT_FOUND.into_response());
    };
    if let Some(response) = redirect_to_canonical(&original_uri, profile_feed_url(profile_id)) {
        return Ok(response);
    }

    // Get profile info
    let profile = state.get_social_profile(profile_id, &client_ref).await;

    // Fetch recent posts (no pagination cursor = most recent)
    // Filter to only top-level posts (not replies)
    let (posts, _) = client_ref
        .db()
        .paginate_social_posts_rev(None, 50, move |post| {
            post.author == profile_id && post.reply_to.is_none()
        })
        .await;

    // Build feed entries
    let mut entries: Vec<Entry> = Vec::new();

    for post in posts {
        let Some(djot_content) = post.content.djot_content.as_ref() else {
            continue;
        };

        // Render content to HTML
        let html_content = state
            .render_content(&client_ref, post.author, djot_content)
            .await;

        // Extract title from djot content
        let excerpt = rostra_djot::extract::extract_excerpt(djot_content);
        let title = excerpt
            .first_heading
            .or(excerpt.first_paragraph)
            .map(|s| crate::layout::truncate_at_word_boundary(&s, 80))
            .unwrap_or_default();

        // Convert timestamp to RFC 3339 format for Atom
        let updated = timestamp_to_rfc3339(post.ts);

        let entry = Entry {
            id: format!("rostra:post:{}:{}", post.author, post.event_id),
            title,
            updated: updated.clone(),
            published: Some(updated),
            authors: vec![Person {
                name: profile.display_name.clone(),
                uri: Some(format!("rostra:profile:{profile_id}")),
                ..Default::default()
            }],
            links: vec![Link {
                href: post_url(post.author, post.event_id),
                rel: Some("alternate".to_string()),
                ..Default::default()
            }],
            content: Some(Content::Html(html_content.0)),
            ..Default::default()
        };

        entries.push(entry);
    }

    // Get most recent update time for the feed
    let updated = entries
        .first()
        .map(|e| e.updated.clone())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    let feed = Feed {
        id: format!("rostra:profile:{profile_id}"),
        title: format!("{} - Rostra", profile.display_name),
        updated,
        entries,
        links: vec![Link {
            href: profile_feed_url(profile_id),
            rel: Some("self".to_string()),
            mediatype: Some("application/atom+xml".to_string()),
            ..Default::default()
        }],
        authors: vec![Person {
            name: profile.display_name.clone(),
            ..Default::default()
        }],
        ..Default::default()
    };

    Ok(AtomFeed(feed).into_response())
}

/// Convert a rostra Timestamp to RFC 3339 format for Atom feeds
fn timestamp_to_rfc3339(ts: rostra_core::Timestamp) -> String {
    // Use the conversion method on Timestamp
    let datetime = ts
        .to_offset_date_time()
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

    // Format as RFC 3339
    let format = time::format_description::well_known::Rfc3339;
    datetime
        .format(&format)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
