use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request;
use rostra_core::id::{RostraId, RostraIdSecretKey};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::UiState;
use crate::error::{
    InternalServerSnafu, LoginRequiredSnafu, ReadOnlyModeSnafu, RequestError, RequestResult,
};

#[derive(Clone, Deserialize, Serialize)]
pub struct UserSession {
    id: RostraId,
    id_secret: Option<RostraIdSecretKey>,
}

impl UserSession {
    pub(crate) fn id(&self) -> RostraId {
        self.id
    }

    pub(crate) fn id_secret(&self) -> RequestResult<RostraIdSecretKey> {
        self.id_secret.ok_or_else(|| ReadOnlyModeSnafu.build())
    }

    pub(crate) fn new(rostra_id: RostraId, secret_key: Option<RostraIdSecretKey>) -> Self {
        Self {
            id: rostra_id,
            id_secret: secret_key,
        }
    }
    pub(crate) fn ro_mode(&self) -> RoMode {
        if self.id_secret.is_some() {
            RoMode::Rw
        } else {
            RoMode::Ro
        }
    }
}

#[derive(Copy, Clone, Debug)]
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
}
// impl Guest {
//     const GUEST_DATA_KEY: &'static str = "guest_data";

//     fn first_seen(&self) -> OffsetDateTime {
//         self.guest_data.first_seen
//     }

//     fn last_seen(&self) -> OffsetDateTime {
//         self.guest_data.last_seen
//     }

//     fn pageviews(&self) -> usize {
//         self.guest_data.pageviews
//     }

//     async fn mark_pageview(&mut self) {
//         self.guest_data.pageviews += 1;
//         Self::update_session(&self.session, &self.guest_data).await
//     }

//     async fn update_session(session: &Session, guest_data:
// &AuthenticatedUser) {         session
//             .insert(Self::GUEST_DATA_KEY, guest_data.clone())
//             .await
//             .unwrap()
//     }
// }

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

        // Try to get the user session from the session store
        let user_result: Result<Option<UserSession>, _> =
            session.get(SESSION_KEY).await.map_err(|_| {
                InternalServerSnafu {
                    msg: "session store error",
                }
                .build()
            });

        match user_result {
            Ok(Some(user)) => Ok(user),
            Ok(None) => {
                if let Some(default_id) = state.default_profile {
                    // Load the client for the default profile in read-only mode
                    state.unlock(default_id, None).await.map_err(|_e| {
                        InternalServerSnafu {
                            msg: "Failed to load default profile",
                        }
                        .build()
                    })?;

                    let user = UserSession::new(default_id, None);

                    session.insert(SESSION_KEY, &user).await.map_err(|_| {
                        InternalServerSnafu {
                            msg: "failed to insert session",
                        }
                        .build()
                    })?;

                    Ok(user)
                } else {
                    // No default profile, require login
                    Err(LoginRequiredSnafu.build())
                }
            }
            Err(e) => Err(e),
        }
    }
}
