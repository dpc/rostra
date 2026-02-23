use std::io;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use rostra_client::ClientRefError;
use rostra_client::error::{ActivateError, InitError, PostError};
use rostra_client::multiclient::MultiClientError;
use rostra_client_db::DbError;
use rostra_core::event::ContentValidationError;
use rostra_util_error::{BoxedError, FmtCompact as _};
use serde::Serialize;
use snafu::Snafu;
use tracing::{debug, warn};

use super::routes::AppJson;
use crate::{LOG_TARGET, UiStateClientError};

/// Walk the error source chain looking for a specific error type.
fn find_in_chain<'a, T: std::error::Error + 'static>(
    e: &'a (dyn std::error::Error + 'static),
) -> Option<&'a T> {
    let mut cur: &dyn std::error::Error = e;
    loop {
        if let Some(t) = cur.downcast_ref::<T>() {
            return Some(t);
        }
        cur = cur.source()?;
    }
}

/// Error by the user
#[derive(Debug, Snafu)]
pub enum UserRequestError {
    SomethingNotFound,
    #[snafu(visibility(pub(crate)))]
    InvalidData,
    #[snafu(visibility(pub(crate)))]
    #[snafu(display("{message}"))]
    BadRequest {
        message: String,
    },
    #[snafu(transparent)]
    ContentValidation {
        source: ContentValidationError,
    },
}

impl IntoResponse for &UserRequestError {
    fn into_response(self) -> Response {
        let (status_code, message) = match self {
            UserRequestError::SomethingNotFound => (StatusCode::NOT_FOUND, self.to_string()),
            UserRequestError::InvalidData => (StatusCode::BAD_REQUEST, self.to_string()),
            UserRequestError::BadRequest { message } => (StatusCode::BAD_REQUEST, message.clone()),
            UserRequestError::ContentValidation { source } => {
                (StatusCode::BAD_REQUEST, source.public_message.clone())
            }
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
pub enum UnlockError {
    #[snafu(visibility(pub(crate)))]
    PublicKeyMissing,
    #[snafu(visibility(pub(crate)))]
    IdMismatch,
    #[snafu(transparent)]
    Io {
        source: io::Error,
    },
    Database {
        source: DbError,
    },
    Init {
        #[snafu(source(from(InitError, Box::new)))]
        source: Box<InitError>,
    },
    #[snafu(transparent)]
    MultiClient {
        #[snafu(source(from(MultiClientError, Box::new)))]
        source: Box<MultiClientError>,
    },
    #[snafu(transparent)]
    MultiClientActivate {
        source: ActivateError,
    },
}
pub type UnlockResult<T> = std::result::Result<T, UnlockError>;

#[derive(Debug, Snafu)]
pub enum RequestError {
    #[snafu(transparent)]
    Client { source: ClientRefError },
    #[snafu(visibility(pub(crate)))]
    StateClient {
        source: UiStateClientError,
        redirect: Option<String>,
    },
    #[snafu(visibility(pub(crate)))]
    Other { source: BoxedError },
    #[snafu(visibility(pub(crate)))]
    #[snafu(display("InternalServerError: {msg}"))]
    InternalServerError { msg: &'static str },
    #[snafu(visibility(pub(crate)))]
    LoginRequired { redirect: Option<String> },
    #[snafu(visibility(pub(crate)))]
    Unlock { source: UnlockError },
    #[snafu(visibility(pub(crate)))]
    ReadOnlyMode,
    #[snafu(visibility(pub(crate)))]
    User { source: UserRequestError },
}
pub type RequestResult<T> = std::result::Result<T, RequestError>;

/// Default From implementation for backwards compatibility (no redirect)
impl From<UiStateClientError> for RequestError {
    fn from(source: UiStateClientError) -> Self {
        RequestError::StateClient {
            source,
            redirect: None,
        }
    }
}

/// Route `PostError::Validation` through `UserRequestError` so it's
/// discoverable by the error-chain walk in `IntoResponse`.
impl From<PostError> for RequestError {
    fn from(source: PostError) -> Self {
        match source {
            PostError::Validation { source } => RequestError::User {
                source: source.into(),
            },
            other => RequestError::Other {
                source: Box::new(other),
            },
        }
    }
}

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        debug!(
            target: LOG_TARGET,
            err = %self.fmt_compact(),
            "Request Error"
        );

        if let Some(user_err) = find_in_chain::<UserRequestError>(&self) {
            return user_err.into_response();
        }

        let (status_code, message) = match self {
            RequestError::StateClient { redirect, .. }
            | RequestError::LoginRequired { redirect } => {
                // Use standard HTTP redirect for Alpine-ajax
                let url = match redirect {
                    Some(ref path) => {
                        format!("/unlock?redirect={}", urlencoding::encode(path))
                    }
                    None => "/unlock".to_string(),
                };
                return Redirect::to(&url).into_response();
            }
            err => {
                warn!(
                    target: LOG_TARGET,
                    err = %err.fmt_compact(),
                    "Unexpected Request Error"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Service Error".to_owned(),
                )
            }
        };

        (status_code, AppJson(UserErrorResponse { message })).into_response()
    }
}
