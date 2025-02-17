use axum::extract::FromRequestParts;
use axum::http::request;
use rostra_core::id::{RostraId, RostraIdSecretKey};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

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

impl<S> FromRequestParts<S> for UserSession
where
    S: Send + Sync,
{
    type Rejection = RequestError;

    async fn from_request_parts(
        req: &mut request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(req, state)
            .await
            .map_err(|(_, msg)| InternalServerSnafu { msg }.build())?;

        let user: UserSession = session
            .get(SESSION_KEY)
            .await
            .map_err(|_| {
                InternalServerSnafu {
                    msg: "session store error",
                }
                .build()
            })?
            .ok_or_else(|| LoginRequiredSnafu.build())?;

        Ok(user)
    }
}
