use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use rostra_core::ShortEventId;
use rostra_core::event::PersonaTag;
use rostra_core::id::{ExternalEventId, RostraId, RostraIdSecretKey};
use serde::{Deserialize, Serialize};

use crate::{SharedState, UiState};

const API_VERSION_HEADER: &str = "x-rostra-api-version";
const API_CURRENT_VERSION: u32 = 0;

const API_SECRET_HEADER: &str = "x-rostra-id-secret";

#[derive(Serialize)]
struct ApiErrorResponse {
    error: String,
}

fn api_error(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ApiErrorResponse>) {
    (status, Json(ApiErrorResponse { error: msg.into() }))
}

type ApiResult<T> = Result<T, (StatusCode, Json<ApiErrorResponse>)>;

// -- Extractors --

/// Extracts and validates the `X-Rostra-Api-Version` header.
struct ApiVersion(#[allow(dead_code)] u32);

impl FromRequestParts<Arc<UiState>> for ApiVersion {
    type Rejection = (StatusCode, Json<ApiErrorResponse>);

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &Arc<UiState>,
    ) -> Result<Self, Self::Rejection> {
        let value = parts.headers.get(API_VERSION_HEADER).ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!("Missing required header: {API_VERSION_HEADER}"),
            )
        })?;
        let version: u32 = value
            .to_str()
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid version header encoding"))?
            .parse()
            .map_err(|_| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    "Version must be a non-negative integer",
                )
            })?;
        if API_CURRENT_VERSION < version {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "Unsupported API version: {version}. Maximum supported: {API_CURRENT_VERSION}"
                ),
            ));
        }
        Ok(ApiVersion(version))
    }
}

/// Extracts the `X-Rostra-Id-Secret` header as a BIP39 mnemonic.
struct ApiIdSecret(RostraIdSecretKey);

impl FromRequestParts<Arc<UiState>> for ApiIdSecret {
    type Rejection = (StatusCode, Json<ApiErrorResponse>);

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &Arc<UiState>,
    ) -> Result<Self, Self::Rejection> {
        let value = parts.headers.get(API_SECRET_HEADER).ok_or_else(|| {
            api_error(
                StatusCode::UNAUTHORIZED,
                format!("Missing required header: {API_SECRET_HEADER}"),
            )
        })?;
        let secret_str = value
            .to_str()
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid secret header encoding"))?;
        let id_secret: RostraIdSecretKey = secret_str.parse().map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Invalid secret key (expected BIP39 mnemonic)",
            )
        })?;
        Ok(ApiIdSecret(id_secret))
    }
}

// -- Router --

pub fn api_router() -> Router<Arc<UiState>> {
    Router::new()
        .route("/generate-id", get(generate_id))
        .route("/{rostra_id}/heads", get(get_heads))
        .route(
            "/{rostra_id}/publish-social-post-managed",
            post(publish_social_post_managed),
        )
}

// -- Endpoints --

#[derive(Serialize)]
struct GenerateIdResponse {
    rostra_id: String,
    rostra_id_secret: String,
}

async fn generate_id(_version: ApiVersion) -> Json<GenerateIdResponse> {
    let secret = RostraIdSecretKey::generate();
    let id = secret.id();
    Json(GenerateIdResponse {
        rostra_id: id.to_string(),
        rostra_id_secret: secret.to_string(),
    })
}

#[derive(Serialize)]
struct HeadsResponse {
    heads: Vec<String>,
}

async fn get_heads(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
) -> ApiResult<Json<HeadsResponse>> {
    state.load_client_api(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client_api(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Client error: {e}"),
        )
    })?;
    let client_ref = client.client_ref().map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Client ref error: {e}"),
        )
    })?;

    let mut heads: Vec<String> = client_ref
        .db()
        .get_heads_events_for_id(rostra_id)
        .await
        .into_iter()
        .take(10)
        .map(|h| h.to_string())
        .collect();
    heads.sort();

    Ok(Json(HeadsResponse { heads }))
}

#[derive(Deserialize)]
struct PublishSocialPostRequest {
    parent_head_id: Option<String>,
    #[serde(default)]
    persona_tags: Vec<String>,
    body: String,
    reply_to: Option<String>,
}

#[derive(Serialize)]
struct PublishSocialPostResponse {
    event_id: String,
    heads: Vec<String>,
}

async fn publish_social_post_managed(
    State(state): State<SharedState>,
    _version: ApiVersion,
    ApiIdSecret(id_secret): ApiIdSecret,
    Path(rostra_id): Path<RostraId>,
    Json(req): Json<PublishSocialPostRequest>,
) -> ApiResult<Json<PublishSocialPostResponse>> {
    // Verify secret matches the rostra_id
    if id_secret.id() != rostra_id {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Secret key does not match the rostra_id",
        ));
    }

    // Load client (before unlock, so heads reflect the caller's view)
    state.load_client_api(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client_api(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Client error: {e}"),
        )
    })?;
    let client_ref = client.client_ref().map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Client ref error: {e}"),
        )
    })?;

    // Get current heads BEFORE unlock_active (which may create a
    // node-announcement event and change heads). This ensures the
    // idempotence check matches what the caller saw via GET /heads.
    let current_heads = client_ref.db().get_heads_events_for_id(rostra_id).await;

    match &req.parent_head_id {
        None => {
            if !current_heads.is_empty() {
                return Err(api_error(
                    StatusCode::CONFLICT,
                    "parent_head_id is null but identity has existing heads. \
                     Call GET /api/{id}/heads to get current heads and pass one as parent_head_id.",
                ));
            }
        }
        Some(head_str) => {
            let head: ShortEventId = head_str
                .parse()
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid parent_head_id format"))?;
            if !current_heads.contains(&head) {
                return Err(api_error(
                    StatusCode::CONFLICT,
                    "parent_head_id is not among current heads. \
                     The post may have already been published, or the state is stale. \
                     Call GET /api/{id}/heads to verify.",
                ));
            }
        }
    }

    // Unlock client (may create a node-announcement event)
    client_ref.unlock_active(id_secret).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unlock error: {e}"),
        )
    })?;

    // Parse persona tags
    let persona_tags: BTreeSet<PersonaTag> = req
        .persona_tags
        .iter()
        .filter_map(|s| PersonaTag::new(s).ok())
        .collect();

    // Parse reply_to
    let reply_to: Option<ExternalEventId> = req
        .reply_to
        .as_deref()
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid reply_to format"))?;

    // Publish the social post
    let verified_event = client_ref
        .social_post(id_secret, req.body, reply_to, persona_tags)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to publish: {e}"),
            )
        })?;

    // Get updated heads
    let mut heads: Vec<String> = client_ref
        .db()
        .get_heads_events_for_id(rostra_id)
        .await
        .into_iter()
        .map(|h| h.to_string())
        .collect();
    heads.sort();

    Ok(Json(PublishSocialPostResponse {
        event_id: ShortEventId::from(verified_event.event_id).to_string(),
        heads,
    }))
}
