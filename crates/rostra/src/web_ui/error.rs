use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use snafu::Snafu;
use tracing::info;

use super::routes::AppJson;

/// Error by the user
#[derive(Debug, Snafu)]
pub enum UserRequestError {
    SomethingNotFound,
}

impl IntoResponse for &UserRequestError {
    fn into_response(self) -> Response {
        let (status_code, message) = match self {
            UserRequestError::SomethingNotFound => (StatusCode::NOT_FOUND, self.to_string()),
        };
        (status_code, AppJson(UserErrorResponse { message })).into_response()
    }
}

// How we want user errors responses to be serialized
#[derive(Serialize)]
pub struct UserErrorResponse {
    pub message: String,
}

#[derive(Debug, Snafu)]
pub enum RequestError {}
pub type RequestResult<T> = std::result::Result<T, RequestError>;

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        info!(err=%self, "Request Error");

        let (status_code, message) =
            if let Some(user_err) = root_cause(&self).downcast_ref::<UserRequestError>() {
                return user_err.into_response();
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Service Error".to_owned(),
                )
            };

        (status_code, AppJson(UserErrorResponse { message })).into_response()
    }
}

fn root_cause<E>(e: &E) -> &(dyn std::error::Error + 'static)
where
    E: std::error::Error + 'static,
{
    let mut cur_source: &dyn std::error::Error = e;

    while let Some(new_source) = cur_source.source() {
        cur_source = new_source;
    }
    cur_source
}
