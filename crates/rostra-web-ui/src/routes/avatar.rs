use axum::extract::{Path, State};
use axum::http::header::{ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect};
use rostra_core::id::RostraId;

use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
    req_headers: HeaderMap,
    Path(avatar_id): Path<RostraId>,
) -> RequestResult<impl IntoResponse> {
    let not_found_resp = Redirect::temporary("/assets/icons/circle-user.svg").into_response();
    let Some(profile) = state
        .client(session.id())
        .await?
        .client_ref()?
        .db()
        .get_social_profile(avatar_id)
        .await
    else {
        return Ok(not_found_resp);
    };

    let Some(avatar) = profile.avatar else {
        return Ok(not_found_resp);
    };

    let mut resp_headers = HeaderMap::new();
    // Add ETag header
    let etag = profile.event_id.to_string();
    resp_headers.insert(
        ETAG,
        HeaderValue::from_str(&etag).expect("ETag should be valid header value"),
    );

    // Check if client already has this version
    if let Some(if_none_match) = req_headers.get(IF_NONE_MATCH) {
        if if_none_match.as_bytes() == etag.as_bytes() {
            return Ok((StatusCode::NOT_MODIFIED, resp_headers).into_response());
        }
    }

    let Ok(mime) = HeaderValue::from_str(&avatar.0) else {
        return Ok(not_found_resp);
    };
    Ok(([(header::CONTENT_TYPE, mime)], avatar.1).into_response())
}
