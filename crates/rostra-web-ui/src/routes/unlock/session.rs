use std::fmt;
use std::fmt::Display;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request;
use data_encoding::BASE64URL_NOPAD;
use rand::random;
use rostra_core::id::RostraId;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::error::{InternalServerSnafu, LoginRequiredSnafu, RequestError};
use crate::{SessionToken, UiState};

/// Data stored in the persistent session store (redb).
///
/// This contains only non-secret session metadata: the RostraId and a random
/// CSRF token. Identity secrets stay in-memory only (in `UiState::secrets`) to
/// avoid persisting them to disk where they could leak.
///
/// The session token is NOT stored here - it's derived from the tower-sessions
/// session ID when the session is extracted.
#[derive(Clone, Deserialize, Serialize)]
pub struct UserSessionData {
    id: RostraId,
    #[serde(default)]
    csrf_token: Option<CsrfToken>,
}

impl UserSessionData {
    pub fn new(rostra_id: RostraId) -> Self {
        Self {
            id: rostra_id,
            csrf_token: Some(CsrfToken::generate()),
        }
    }
}

/// Opaque per-session token for confidentiality-sensitive form submissions.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct CsrfToken(String);

impl CsrfToken {
    fn generate() -> Self {
        Self(BASE64URL_NOPAD.encode(&random::<[u8; 32]>()))
    }
}

impl Display for CsrfToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// User session with both the RostraId and session token.
///
/// This is created by the extractor by combining `UserSessionData` (from the
/// session store) with the tower-sessions session ID. Since the session was
/// already saved when the user logged in, the session ID is always available.
#[derive(Clone)]
pub struct UserSession {
    id: RostraId,
    /// The session token, derived from the tower-sessions session ID.
    /// Used to key the in-memory secret storage.
    session_token: SessionToken,
    /// Random request token used to protect confidentiality-sensitive POSTs.
    csrf_token: CsrfToken,
}

impl UserSession {
    pub(crate) fn id(&self) -> RostraId {
        self.id
    }

    /// Returns the session token used to key secret storage.
    pub(crate) fn session_token(&self) -> SessionToken {
        self.session_token
    }

    /// Return the per-session CSRF token.
    pub(crate) fn csrf_token(&self) -> &CsrfToken {
        &self.csrf_token
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RoMode {
    Ro,
    Rw,
}

impl RoMode {
    pub fn to_disabled(self) -> bool {
        match self {
            RoMode::Ro => true,
            RoMode::Rw => false,
        }
    }

    pub fn is_ro(self) -> bool {
        self == Self::Ro
    }
}

pub const SESSION_KEY: &str = "rostra_id";

impl FromRequestParts<Arc<UiState>> for UserSession {
    type Rejection = RequestError;

    async fn from_request_parts(
        req: &mut request::Parts,
        state: &Arc<UiState>,
    ) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(req, state)
            .await
            .map_err(|(_, msg)| InternalServerSnafu { msg }.build())?;

        // Try to get the user session data from the session store.
        let data_result: Result<Option<UserSessionData>, _> =
            session.get(SESSION_KEY).await.map_err(|_| {
                InternalServerSnafu {
                    msg: "session store error",
                }
                .build()
            });

        // Capture request path for potential redirect after login
        let request_path = req.uri.path_and_query().map(|pq| pq.to_string());

        match data_result {
            Ok(Some(mut data)) => {
                // Session exists - check if client is loaded in memory.
                // If not, redirect to unlock page with the original path.
                if !state.is_client_loaded(data.id).await {
                    return Err(LoginRequiredSnafu {
                        redirect: request_path,
                    }
                    .build());
                }

                // Get the session token from tower-sessions ID.
                // This should always be available since the session was saved when user logged
                // in.
                let session_token = SessionToken::from_session(&session).ok_or_else(|| {
                    InternalServerSnafu {
                        msg: "session has no ID",
                    }
                    .build()
                })?;
                let csrf_token = match data.csrf_token.clone() {
                    Some(token) => token,
                    None => {
                        let token = CsrfToken::generate();
                        data.csrf_token = Some(token.clone());
                        session.insert(SESSION_KEY, &data).await.map_err(|_| {
                            InternalServerSnafu {
                                msg: "failed to update session",
                            }
                            .build()
                        })?;
                        token
                    }
                };

                Ok(UserSession {
                    id: data.id,
                    session_token,
                    csrf_token,
                })
            }
            Ok(None) => {
                if let Some(default_id) = state.default_profile {
                    // Load the client for the default profile in read-only mode.
                    // No secret is stored since this is read-only.
                    state.load_client(default_id).await.map_err(|_e| {
                        InternalServerSnafu {
                            msg: "Failed to load default profile",
                        }
                        .build()
                    })?;

                    // Insert session data and save to store
                    let data = UserSessionData::new(default_id);
                    session.insert(SESSION_KEY, &data).await.map_err(|_| {
                        InternalServerSnafu {
                            msg: "failed to insert session",
                        }
                        .build()
                    })?;
                    session.save().await.map_err(|_| {
                        InternalServerSnafu {
                            msg: "failed to save session",
                        }
                        .build()
                    })?;

                    // Now get the session token (available after save)
                    let session_token = SessionToken::from_session(&session).ok_or_else(|| {
                        InternalServerSnafu {
                            msg: "session has no ID after save",
                        }
                        .build()
                    })?;

                    Ok(UserSession {
                        id: default_id,
                        session_token,
                        csrf_token: data.csrf_token.expect("new sessions have a CSRF token"),
                    })
                } else {
                    // No default profile, require login
                    Err(LoginRequiredSnafu {
                        redirect: request_path,
                    }
                    .build())
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// Optional user session - returns None instead of redirecting if not
/// authenticated. Use this for routes that need to behave differently for auth
/// vs non-auth users.
///
/// Unlike `UserSession`, this does NOT auto-load the default profile.
/// It only returns `Some` if the user has an existing session.
pub struct OptionalUserSession(pub Option<UserSession>);

impl FromRequestParts<Arc<UiState>> for OptionalUserSession {
    type Rejection = RequestError;

    async fn from_request_parts(
        req: &mut request::Parts,
        state: &Arc<UiState>,
    ) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(req, state)
            .await
            .map_err(|(_, msg)| InternalServerSnafu { msg }.build())?;

        let data_result: Result<Option<UserSessionData>, _> =
            session.get(SESSION_KEY).await.map_err(|_| {
                InternalServerSnafu {
                    msg: "session store error",
                }
                .build()
            });

        match data_result {
            Ok(Some(data)) => {
                // Try to get session token - if not available, treat as no session
                let session_token = SessionToken::from_session(&session);
                match session_token {
                    Some(token) => Ok(OptionalUserSession(Some(UserSession {
                        id: data.id,
                        session_token: token,
                        csrf_token: data.csrf_token.unwrap_or_else(CsrfToken::generate),
                    }))),
                    None => Ok(OptionalUserSession(None)),
                }
            }
            Ok(None) => Ok(OptionalUserSession(None)),
            Err(e) => Err(e),
        }
    }
}
