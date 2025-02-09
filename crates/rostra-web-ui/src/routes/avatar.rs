use axum::extract::{Path, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Redirect};
use rostra_core::id::RostraId;

use super::unlock::session::UserSession;
use crate::error::RequestResult;
use crate::SharedState;

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
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
    let Ok(mime) = HeaderValue::from_str(&avatar.0) else {
        return Ok(not_found_resp);
    };
    Ok(([(header::CONTENT_TYPE, mime)], avatar.1).into_response())
}
