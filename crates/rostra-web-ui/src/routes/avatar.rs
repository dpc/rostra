use axum::body::Body;
use axum::extract::{OriginalUri, Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum_dpc_static_assets::handle_etag;
use rostra_core::id::RostraId;

use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;
use crate::routes::url::{RostraPathId, avatar_path, redirect_to_canonical};

const DEFAULT_AVATAR_SVG: &[u8] = include_bytes!("../../assets/icons/circle-user.svg");
const DEFAULT_AVATAR_ETAG: &str = "default-circle-user-svg";

fn serve_default_avatar(req_headers: &HeaderMap) -> Response<Body> {
    let mut resp_headers = HeaderMap::new();

    if let Some(response) = handle_etag(req_headers, DEFAULT_AVATAR_ETAG, &mut resp_headers) {
        return response.into_response();
    }

    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml"),
    );
    (resp_headers, DEFAULT_AVATAR_SVG).into_response()
}

async fn serve_avatar(
    state: &State<SharedState>,
    session: &UserSession,
    req_headers: &HeaderMap,
    avatar_id: RostraId,
) -> RequestResult<Response<Body>> {
    let Some(profile) = state
        .client(session.id())
        .await?
        .client_ref()?
        .db()
        .get_social_profile(avatar_id)
        .await
    else {
        return Ok(serve_default_avatar(req_headers));
    };

    let Some(avatar) = profile.avatar else {
        return Ok(serve_default_avatar(req_headers));
    };

    let mut resp_headers = HeaderMap::new();
    let etag = profile.event_id.to_string();

    if let Some(response) = handle_etag(req_headers, &etag, &mut resp_headers) {
        return Ok(response.into_response());
    }

    let Ok(mime) = HeaderValue::from_str(&avatar.0) else {
        return Ok(serve_default_avatar(req_headers));
    };
    resp_headers.insert(header::CONTENT_TYPE, mime);
    Ok((resp_headers, avatar.1).into_response())
}

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
    req_headers: HeaderMap,
    OriginalUri(original_uri): OriginalUri,
    Path(avatar_id): Path<RostraPathId>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let Some(avatar_id) = avatar_id.resolve(client_ref.db()).await else {
        return Ok(axum::http::StatusCode::NOT_FOUND.into_response());
    };
    if let Some(response) = redirect_to_canonical(&original_uri, avatar_path(avatar_id)) {
        return Ok(response);
    }
    serve_avatar(&state, &session, &req_headers, avatar_id).await
}
