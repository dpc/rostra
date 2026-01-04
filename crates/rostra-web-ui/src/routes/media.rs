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

use super::Maud;
use super::unlock::session::UserSession;
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

                let _event = client_ref
                    .publish_event(session.id_secret()?, media_event)
                    .call()
                    .await?;

                return Ok(Maud(html! {
                    div id="ajax-scripts" {
                        script {
                            (PreEscaped(r#"
                                window.dispatchEvent(new CustomEvent('notify', {
                                    detail: { type: 'success', message: 'Media uploaded successfully' }
                                }));
                            "#))
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

pub async fn list(
    state: State<SharedState>,
    session: UserSession,
    Path(author): Path<RostraId>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    // Get all SocialMedia events for this user from events_singleton2 table
    // This should work for different images as they have different hashes
    let media_events = client_ref
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

                events.push((value.ts, value.inner.event_id, value.inner.clone()));
            }

            events.sort_by_key(|val| val.0);

            Ok(events)
        })
        .await
        .unwrap_or_default();

    Ok(Maud(html! {
        div id="media-list" ."o-mediaList -active" {
            div ."o-mediaList__content" {
                h3 { "Select media to attach:" }
                div ."o-mediaList__items" {
                    @if media_events.is_empty() {
                        div ."o-mediaList__empty" {
                            "No media files uploaded yet."
                        }
                    } @else {
                        @for (_ts, event_id, _event_record) in media_events {
                            div ."o-mediaList__item"
                                onclick=(format!("insertMediaSyntax('{}'); document.getElementById('media-list').classList.remove('-active')", event_id.to_short()))
                            {
                                img
                                    src=(format!("/ui/media/{}/{}", author, event_id.to_short()))
                                    ."o-mediaList__thumbnail"
                                    loading="lazy"
                                    {}
                            }
                        }
                    }
                }
                div ."o-mediaList__actionButtons" {
                    button ."o-mediaList__closeButton u-button"
                        type="button"
                        onclick="document.getElementById('media-list').classList.remove('-active')"
                    {
                        "Close"
                    }
                }
            }
        }
    }))
}
