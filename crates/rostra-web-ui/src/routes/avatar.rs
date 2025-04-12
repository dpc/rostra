use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use rostra_core::id::RostraId;

use super::get_static_asset;
use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
    req_headers: HeaderMap,
    Path(avatar_id): Path<RostraId>,
) -> RequestResult<Response<Body>> {
    let not_found_redirect = Redirect::temporary("/assets/icons/circle-user.svg").into_response();
    let Some(profile) = state
        .client(session.id())
        .await?
        .client_ref()?
        .db()
        .get_social_profile(avatar_id)
        .await
    else {
        if state.assets.is_some() {
            return Ok(get_static_asset(
                state,
                Path("icons/circle-user.svg".to_owned()),
                req_headers,
            )
            .await
            .into_response());
        } else {
            return Ok(not_found_redirect.into_response());
        }
    };
    let Some(avatar) = profile.avatar else {
        if state.assets.is_some() {
            return Ok(get_static_asset(
                state,
                Path("icons/circle-user.svg".to_owned()),
                req_headers,
            )
            .await
            .into_response());
        } else {
            return Ok(not_found_redirect.into_response());
        }
    };

    let mut resp_headers = HeaderMap::new();
    let etag = profile.event_id.to_string();

    // Handle ETag and conditional request
    if let Some(response) = crate::handle_etag(&req_headers, &etag, &mut resp_headers) {
        return Ok(response.into_response());
    }

    let Ok(mime) = HeaderValue::from_str(&avatar.0) else {
        return Ok(not_found_redirect.into_response());
    };
    resp_headers.insert(header::CONTENT_TYPE, mime);
    Ok((resp_headers, avatar.1).into_response())
}
