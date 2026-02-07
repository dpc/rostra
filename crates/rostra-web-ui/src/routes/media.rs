use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum_dpc_static_assets::handle_etag;
use maud::{PreEscaped, html};
use rostra_client_db::events_singletons_new;
use rostra_core::ShortEventId;
use rostra_core::event::content_kind;
use rostra_core::id::{RostraId, ToShort as _};
use snafu::ResultExt as _;

use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::SharedState;
use crate::error::{OtherSnafu, RequestResult};

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
    req_headers: HeaderMap,
    Path((_author, event_id)): Path<(RostraId, ShortEventId)>,
) -> RequestResult<Response<Body>> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    // Look up the event content
    let Some(event_content) = client_ref.db().get_event_content(event_id).await else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    // Deserialize as SocialMedia content
    let media_content: content_kind::SocialMedia = match event_content.deserialize_cbor() {
        Ok(content) => content,
        Err(_) => return Ok(StatusCode::BAD_REQUEST.into_response()),
    };

    let mut resp_headers = HeaderMap::new();
    let etag = event_id.to_string();

    // Handle ETag and conditional request
    if let Some(response) = handle_etag(&req_headers, &etag, &mut resp_headers) {
        return Ok(response.into_response());
    }

    // Set content type from the media's MIME type
    let Ok(mime) = HeaderValue::from_str(&media_content.mime) else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };
    resp_headers.insert(header::CONTENT_TYPE, mime);

    // Return the media data
    Ok((resp_headers, media_content.data).into_response())
}

pub async fn publish(
    state: State<SharedState>,
    session: UserSession,
    mut multipart: Multipart,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    // Process the multipart form data
    while let Some(field) = multipart.next_field().await.boxed().context(OtherSnafu)? {
        // Check if this is the media_file field
        if field.name() == Some("media_file") {
            if let Some(_file_name) = field.file_name() {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let data = field.bytes().await.boxed().context(OtherSnafu)?;

                // Limit file size to 10MB
                if data.len() > 10 * 1024 * 1024 {
                    return Ok(Maud(html! {
                        div id="ajax-scripts" {
                            script {
                                (PreEscaped(r#"
                                    window.dispatchEvent(new CustomEvent('notify', {
                                        detail: { type: 'error', message: 'File too large. Maximum size is 10MB.' }
                                    }));
                                "#))
                            }
                        }
                    }));
                }

                // Create and publish SocialMedia event
                let media_event = content_kind::SocialMedia {
                    mime: content_type,
                    data: data.to_vec(),
                };

                let event = client_ref
                    .publish_event(session.id_secret()?, media_event)
                    .call()
                    .await?;

                let event_id = event.event_id.to_short();
                return Ok(Maud(html! {
                    div id="ajax-scripts" {
                        script {
                            (PreEscaped(format!(r#"
                                insertMediaSyntax('{}');
                                window.dispatchEvent(new CustomEvent('notify', {{
                                    detail: {{ type: 'success', message: 'Media uploaded and inserted' }}
                                }}));
                            "#, event_id)))
                        }
                    }
                }));
            }
        }
    }

    Ok(Maud(html! {
        div id="ajax-scripts" {
            script {
                (PreEscaped(r#"
                    window.dispatchEvent(new CustomEvent('notify', {
                        detail: { type: 'error', message: 'No file selected' }
                    }));
                "#))
            }
        }
    }))
}

/// Information about a media item for display
struct MediaInfo {
    event_id: ShortEventId,
    mime: String,
    size: usize,
    is_image: bool,
    is_video: bool,
}

pub async fn list(
    state: State<SharedState>,
    session: UserSession,
    Path(author): Path<RostraId>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    // Get all SocialMedia events for this user from events_singleton2 table
    // This should work for different images as they have different hashes
    let media_event_ids: Vec<ShortEventId> = client_ref
        .db()
        .read_with(|tx| {
            let singletons_table = tx.open_table(&events_singletons_new::TABLE)?;

            let mut events = Vec::new();
            let kind = rostra_core::event::EventKind::SOCIAL_MEDIA;

            // Use a targeted range query to get only SOCIAL_MEDIA events for this user
            let range_start = (author, kind, rostra_core::event::EventAuxKey::ZERO);
            let range_end = (author, kind, rostra_core::event::EventAuxKey::MAX);

            for record in singletons_table.range(range_start..=range_end)? {
                let record = record?;
                let key = record.0.value();
                let value = record.1.value();

                // Verify this is exactly what we want (should always be true with the range)
                debug_assert_eq!(key.0, author);
                debug_assert_eq!(key.1, kind);

                events.push((value.ts, value.inner.event_id));
            }

            events.sort_by_key(|val: &(rostra_core::Timestamp, ShortEventId)| val.0);

            Ok(events.into_iter().map(|(_, id)| id).collect())
        })
        .await
        .unwrap_or_default();

    // Fetch media info for each event
    let mut media_items = Vec::new();
    for event_id in media_event_ids {
        if let Some(event_content) = client_ref.db().get_event_content(event_id).await {
            if let Ok(media_content) = event_content.deserialize_cbor::<content_kind::SocialMedia>()
            {
                let is_image = media_content.mime.starts_with("image/");
                let is_video = media_content.mime.starts_with("video/");
                media_items.push(MediaInfo {
                    event_id,
                    mime: media_content.mime,
                    size: media_content.data.len(),
                    is_image,
                    is_video,
                });
            }
        }
    }

    Ok(Maud(html! {
        div id="media-list" ."o-mediaList -active" {
            (fragment::dialog_escape_handler("media-list"))
            div ."o-mediaList__content" {
                h4 ."o-mediaList__title" { "Select media to attach" }
                div ."o-mediaList__items" {
                    @if media_items.is_empty() {
                        div ."o-mediaList__empty" {
                            "No media files uploaded yet."
                        }
                    } @else {
                        @for media in &media_items {
                            div ."o-mediaList__item"
                                onclick=(format!("insertMediaSyntax('{}'); document.getElementById('media-list').classList.remove('-active')", media.event_id))
                            {
                                @if media.is_image {
                                    img
                                        src=(format!("/ui/media/{}/{}", author, media.event_id))
                                        ."o-mediaList__thumbnail"
                                        loading="lazy"
                                        {}
                                } @else if media.is_video {
                                    video
                                        src=(format!("/ui/media/{}/{}", author, media.event_id))
                                        ."o-mediaList__videoThumbnail"
                                        autoplay
                                        muted
                                        loop
                                        playsinline
                                        {}
                                } @else {
                                    div ."o-mediaList__fileInfo" {
                                        div ."o-mediaList__fileIcon" {}
                                        div ."o-mediaList__fileMeta" {
                                            div ."o-mediaList__fileMime" { (media.mime.as_str()) }
                                            div ."o-mediaList__fileSize" { (rostra_util_fmt::format_bytes(media.size as u64)) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div ."o-mediaList__actionButtons" {
                    (fragment::button("o-mediaList__closeButton", "Close")
                        .button_type("button")
                        .onclick("document.getElementById('media-list').classList.remove('-active')")
                        .call())
                }
            }
        }
    }))
}
