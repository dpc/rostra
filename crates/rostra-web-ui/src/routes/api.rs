use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use rostra_client_db::social::{EventPaginationCursor, ReceivedAtPaginationCursor};
use rostra_core::event::{
    Event, EventContentRaw, EventSignature, PersonaTag, PersonasTagsSelector, SignedEvent,
    SocialPost, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{ExternalEventId, RostraId, RostraIdSecretKey};
use rostra_core::{ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::routes::media_type::verify_avatar;
use crate::{SharedState, UiState};

const API_VERSION_HEADER: &str = "x-rostra-api-version";
const API_CURRENT_VERSION: u32 = 0;

const API_SECRET_HEADER: &str = "x-rostra-id-secret";
/// Pin the ordinary Axum JSON limit so pre-parse capacity never trusts headers.
const SIGNED_EVENT_BODY_LIMIT: usize = 2 * 1024 * 1024;

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
        .route(
            "/{rostra_id}/update-social-profile-managed",
            post(update_social_profile_managed),
        )
        .route(
            "/{rostra_id}/publish-social-post-prepare",
            post(publish_social_post_prepare),
        )
        .route(
            "/{rostra_id}/publish",
            post(publish_signed_event).layer(DefaultBodyLimit::max(SIGNED_EVENT_BODY_LIMIT)),
        )
        .route("/{rostra_id}/follow-managed", post(follow_managed))
        .route("/{rostra_id}/unfollow-managed", post(unfollow_managed))
        .route("/{rostra_id}/followees", get(get_followees))
        .route("/{rostra_id}/followers", get(get_followers))
        .route("/{rostra_id}/notifications", get(get_notifications))
        .route("/{rostra_id}/posts", get(get_posts_by_author))
        .route("/{rostra_id}/posts/{event_id}", get(get_single_post))
        .route("/{rostra_id}/following", get(get_following_timeline))
        .route("/{rostra_id}/network", get(get_network_timeline))
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
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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
    content: String,
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
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let expected_head = req
        .parent_head_id
        .as_deref()
        .map(str::parse::<ShortEventId>)
        .transpose()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid parent_head_id format"))?;

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
        .social_post_if_head(
            id_secret,
            expected_head,
            req.content,
            reply_to,
            persona_tags,
        )
        .await
        .map_err(|e| {
            if matches!(
                e,
                rostra_client::error::PostError::Storage {
                    source: rostra_client_db::DbError::SocialPostHeadConflict,
                }
            ) {
                return api_error(
                    StatusCode::CONFLICT,
                    if expected_head.is_some() {
                        "parent_head_id is not among current heads. \
                     The post may have already been published, or the state is stale. \
                     Call GET /api/{id}/heads to verify."
                    } else {
                        "parent_head_id is null but identity has existing heads. \
                     Call GET /api/{id}/heads to get current heads and pass one as parent_head_id."
                    },
                );
            }
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to publish: {e}"),
            )
        })?;

    // Publish before activation: initialization itself can create graph heads.
    // Failure/cancellation here does not roll back the already consumed head.
    client_ref.unlock_active(id_secret).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Post committed, but activation failed: {e}"),
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

// -- Update Social Profile --

#[derive(Deserialize)]
struct AvatarData {
    mime_type: String,
    base64: String,
}

#[derive(Deserialize)]
struct UpdateSocialProfileRequest {
    display_name: String,
    bio: String,
    avatar: Option<AvatarData>,
}

#[derive(Serialize)]
struct UpdateSocialProfileResponse {
    event_id: String,
    heads: Vec<String>,
}

async fn update_social_profile_managed(
    State(state): State<SharedState>,
    _version: ApiVersion,
    ApiIdSecret(id_secret): ApiIdSecret,
    Path(rostra_id): Path<RostraId>,
    Json(req): Json<UpdateSocialProfileRequest>,
) -> ApiResult<Json<UpdateSocialProfileResponse>> {
    // Verify secret matches the rostra_id
    if id_secret.id() != rostra_id {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Secret key does not match the rostra_id",
        ));
    }

    // Load and unlock client
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    client_ref.unlock_active(id_secret).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unlock error: {e}"),
        )
    })?;

    // Decode avatar if provided
    let avatar = match req.avatar {
        Some(avatar_data) => {
            let bytes = data_encoding::BASE64
                .decode(avatar_data.base64.as_bytes())
                .or_else(|_| data_encoding::BASE64_NOPAD.decode(avatar_data.base64.as_bytes()))
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid base64 in avatar data"))?;
            if verify_avatar(&avatar_data.mime_type, &bytes).is_none() {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    "Avatar bytes must match a supported image MIME type",
                ));
            }
            Some((avatar_data.mime_type, bytes))
        }
        None => {
            // Preserve existing avatar when not provided
            client_ref
                .db()
                .get_social_profile(rostra_id)
                .await
                .and_then(|p| p.avatar)
        }
    };

    // Publish profile update
    let verified_event = client_ref
        .post_social_profile_update(id_secret, req.display_name, req.bio, avatar)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to update profile: {e}"),
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

    Ok(Json(UpdateSocialProfileResponse {
        event_id: ShortEventId::from(verified_event.event_id).to_string(),
        heads,
    }))
}

// -- Secretless publish --

#[derive(Serialize)]
struct PublishSocialPostPrepareResponse {
    event: Event,
    content: EventContentRaw,
}

async fn publish_social_post_prepare(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
    Json(req): Json<PublishSocialPostRequest>,
) -> ApiResult<Json<PublishSocialPostPrepareResponse>> {
    // Load client
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    // Idempotency check (same pattern as managed endpoint)
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

    // Fetch DAG state
    let current_head = client_ref.db().get_self_current_head().await;
    let aux_event = client_ref.db().get_self_random_eventid().await;

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

    // Build unsigned event + content
    let social_post = SocialPost::new(req.content, reply_to, persona_tags);
    let (event, content) = Event::builder(&social_post)
        .author(rostra_id)
        .maybe_parent_prev(current_head)
        .maybe_parent_aux(aux_event)
        .build()
        .map_err(|e| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!("Content validation failed: {e}"),
            )
        })?;

    Ok(Json(PublishSocialPostPrepareResponse { event, content }))
}

#[derive(Deserialize)]
struct PublishSignedEventRequest {
    event: Event,
    sig: EventSignature,
    content: EventContentRaw,
}

#[derive(Serialize)]
struct PublishSignedEventResponse {
    event_id: String,
    heads: Vec<String>,
}

async fn publish_signed_event(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
    request: Request,
) -> ApiResult<Json<PublishSignedEventResponse>> {
    // Never create/open a database from an unverified path or malformed body.
    // Startup configuration is immutable and available before lazy loading.
    let account = state.clients.payload_account(rostra_id);
    let existing_client = state.client(rostra_id).await.ok();
    let existing_ref = existing_client
        .as_ref()
        .and_then(|client| client.client_ref().ok());
    let capacity_error = |reason| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Payload storage capacity unavailable: {reason:?}; retry later"),
        )
    };
    // Body, JSON string/scratch, and Vec -> Arc conversion can coexist.
    // Codec/transport scratch and allocator overhead are not measured here.
    let reserve = || match (&account, &existing_ref) {
        (Some(account), _) => account.reserve_payload_allocation(SIGNED_EVENT_BODY_LIMIT as u64),
        (None, Some(client)) => client
            .db()
            .reserve_payload_allocation(SIGNED_EVENT_BODY_LIMIT as u64),
        (None, None) => Ok(None),
    };
    let body_capacity = reserve().map_err(capacity_error)?;
    let string_capacity = reserve().map_err(capacity_error)?;
    let mut payload_capacity = reserve().map_err(capacity_error)?;
    let conversion_capacity = reserve().map_err(capacity_error)?;
    let scratch_capacity = reserve().map_err(capacity_error)?;
    let parse = Json::<PublishSignedEventRequest>::from_request(request, &state);
    let parsed = if payload_capacity.is_some() {
        tokio::time::timeout(std::time::Duration::from_secs(30), parse)
            .await
            .map_err(|_| api_error(StatusCode::REQUEST_TIMEOUT, "Payload body read timed out"))?
    } else {
        parse.await
    };
    let Json(req) = parsed.map_err(|err| api_error(err.status(), err.body_text()))?;
    drop(body_capacity);
    drop(string_capacity);
    drop(conversion_capacity);
    drop(scratch_capacity);
    // Verify author matches the path
    if req.event.author != rostra_id {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Event author does not match the rostra_id in the URL",
        ));
    }

    // Verify signature
    let signed_event = SignedEvent::unverified(req.event, req.sig);
    let verified_event = VerifiedEvent::verify_received_as_is(signed_event).map_err(|e| {
        api_error(
            StatusCode::BAD_REQUEST,
            format!("Signature verification failed: {e}"),
        )
    })?;

    // Verify content matches event hash/len
    let verified_event_content = VerifiedEventContent::verify(verified_event, req.content)
        .map_err(|e| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!("Content verification failed: {e}"),
            )
        })?;

    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;
    let client = state.client(rostra_id).await.map_err(|e| {
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

    let storage_error = |e| {
        if let rostra_client_db::DbError::PayloadAdmissionPaused { reason } = e {
            return capacity_error(reason);
        }
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to store event: {e}"),
        )
    };
    if let Some(payload_capacity) = &mut payload_capacity {
        let buffer = match client_ref
            .db()
            .reserve_payload(&verified_event)
            .await
            .map_err(storage_error)?
        {
            rostra_client_db::PayloadReservationOutcome::Reserved(reservation) => Some(
                payload_capacity
                    .bind_to_reservation(&reservation)
                    .map_err(capacity_error)?,
            ),
            rostra_client_db::PayloadReservationOutcome::Deferred(reason) => {
                return Err(capacity_error(reason));
            }
            _ => None,
        };
        client_ref
            .store_acquired_payload(verified_event_content, buffer)
            .await
            .map_err(storage_error)?;
    } else {
        client_ref
            .db()
            .try_process_event_with_content(&verified_event_content)
            .await
            .map_err(storage_error)?;
    }

    // Get updated heads
    let mut heads: Vec<String> = client_ref
        .db()
        .get_heads_events_for_id(rostra_id)
        .await
        .into_iter()
        .map(|h| h.to_string())
        .collect();
    heads.sort();

    Ok(Json(PublishSignedEventResponse {
        event_id: ShortEventId::from(verified_event.event_id).to_string(),
        heads,
    }))
}

// -- Follow / Unfollow --

#[derive(Deserialize)]
struct FollowManagedRequest {
    followee: String,
    /// "only" or "except" (defaults to "except" = follow all)
    #[serde(default)]
    filter_mode: Option<String>,
    /// Persona tags for the filter
    #[serde(default)]
    persona_tags: Vec<String>,
}

#[derive(Serialize)]
struct FollowManagedResponse {
    event_id: String,
    heads: Vec<String>,
}

async fn follow_managed(
    State(state): State<SharedState>,
    _version: ApiVersion,
    ApiIdSecret(id_secret): ApiIdSecret,
    Path(rostra_id): Path<RostraId>,
    Json(req): Json<FollowManagedRequest>,
) -> ApiResult<Json<FollowManagedResponse>> {
    if id_secret.id() != rostra_id {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Secret key does not match the rostra_id",
        ));
    }

    let followee: RostraId = req
        .followee
        .parse()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid followee rostra_id"))?;

    let tags: BTreeSet<PersonaTag> = req
        .persona_tags
        .iter()
        .filter_map(|s| PersonaTag::new(s).ok())
        .collect();

    let selector = match req.filter_mode.as_deref() {
        Some("only") => PersonasTagsSelector::Only { ids: tags },
        _ => PersonasTagsSelector::Except { ids: tags },
    };

    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    client_ref.unlock_active(id_secret).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unlock error: {e}"),
        )
    })?;

    let verified_event = client_ref
        .follow(id_secret, followee, selector)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to follow: {e}"),
            )
        })?;

    let mut heads: Vec<String> = client_ref
        .db()
        .get_heads_events_for_id(rostra_id)
        .await
        .into_iter()
        .map(|h| h.to_string())
        .collect();
    heads.sort();

    Ok(Json(FollowManagedResponse {
        event_id: ShortEventId::from(verified_event.event_id).to_string(),
        heads,
    }))
}

#[derive(Deserialize)]
struct UnfollowManagedRequest {
    followee: String,
}

async fn unfollow_managed(
    State(state): State<SharedState>,
    _version: ApiVersion,
    ApiIdSecret(id_secret): ApiIdSecret,
    Path(rostra_id): Path<RostraId>,
    Json(req): Json<UnfollowManagedRequest>,
) -> ApiResult<Json<FollowManagedResponse>> {
    if id_secret.id() != rostra_id {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Secret key does not match the rostra_id",
        ));
    }

    let followee: RostraId = req
        .followee
        .parse()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid followee rostra_id"))?;

    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    client_ref.unlock_active(id_secret).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unlock error: {e}"),
        )
    })?;

    let verified_event = client_ref
        .unfollow(id_secret, followee)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to unfollow: {e}"),
            )
        })?;

    let mut heads: Vec<String> = client_ref
        .db()
        .get_heads_events_for_id(rostra_id)
        .await
        .into_iter()
        .map(|h| h.to_string())
        .collect();
    heads.sort();

    Ok(Json(FollowManagedResponse {
        event_id: ShortEventId::from(verified_event.event_id).to_string(),
        heads,
    }))
}

// -- Followees / Followers --

#[derive(Serialize)]
struct FolloweeItem {
    rostra_id: String,
    filter_mode: String,
    persona_tags: Vec<String>,
}

#[derive(Serialize)]
struct FolloweesResponse {
    followees: Vec<FolloweeItem>,
}

async fn get_followees(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
) -> ApiResult<Json<FolloweesResponse>> {
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let followees_data = client_ref.db().get_followees(rostra_id).await;

    let followees = followees_data
        .into_iter()
        .map(|(id, selector)| {
            let (filter_mode, tags) = match selector {
                PersonasTagsSelector::Only { ids } => ("only", ids),
                PersonasTagsSelector::Except { ids } => ("except", ids),
            };
            FolloweeItem {
                rostra_id: id.to_string(),
                filter_mode: filter_mode.to_string(),
                persona_tags: tags.into_iter().map(|t| t.to_string()).collect(),
            }
        })
        .collect();

    Ok(Json(FolloweesResponse { followees }))
}

#[derive(Serialize)]
struct FollowersResponse {
    followers: Vec<String>,
}

async fn get_followers(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
) -> ApiResult<Json<FollowersResponse>> {
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let followers_data = client_ref.db().get_followers(rostra_id).await;

    let followers = followers_data
        .into_iter()
        .map(|id| id.to_string())
        .collect();

    Ok(Json(FollowersResponse { followers }))
}

// -- Notifications --

#[derive(Deserialize)]
struct NotificationsQuery {
    ts: Option<Timestamp>,
    seq: Option<u64>,
}

#[derive(Serialize)]
struct NotificationItem {
    event_id: String,
    author: String,
    ts: u64,
    content: Option<String>,
    reply_to: Option<String>,
    persona_tags: Vec<String>,
    reply_count: u64,
}

#[derive(Serialize)]
struct NotificationsResponse {
    notifications: Vec<NotificationItem>,
    next_cursor: Option<NotificationsCursor>,
}

#[derive(Serialize)]
struct NotificationsCursor {
    ts: u64,
    seq: u64,
}

async fn get_notifications(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
    Query(query): Query<NotificationsQuery>,
) -> ApiResult<Json<NotificationsResponse>> {
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let cursor = query
        .ts
        .and_then(|ts| query.seq.map(|seq| ReceivedAtPaginationCursor { ts, seq }));

    let self_id = rostra_id;
    let self_mentions = client_ref.db().get_self_mentions().await;

    let (posts, next) = client_ref
        .db()
        .paginate_social_posts_by_received_at_rev(cursor, 20, move |post| {
            post.author != self_id
                && (post.reply_to.map(|ext_id| ext_id.rostra_id()) == Some(self_id)
                    || self_mentions.contains(&post.event_id))
        })
        .await;

    let notifications = posts
        .into_iter()
        .map(|post| {
            let persona_tags = post
                .content
                .persona_tags()
                .into_iter()
                .map(|t| t.to_string())
                .collect();
            NotificationItem {
                event_id: post.event_id.to_string(),
                author: post.author.to_string(),
                ts: post.ts.as_u64(),
                content: post.content.djot_content,
                reply_to: post.reply_to.map(|r| r.to_string()),
                persona_tags,
                reply_count: post.reply_count,
            }
        })
        .collect();

    let next_cursor = next.map(|c| NotificationsCursor {
        ts: c.ts.as_u64(),
        seq: c.seq,
    });

    Ok(Json(NotificationsResponse {
        notifications,
        next_cursor,
    }))
}

// -- Posts by author / Single post --

async fn get_posts_by_author(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<Json<TimelineResponse>> {
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let cursor = query.ts.and_then(|ts| {
        query
            .event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
    });

    let author_id = rostra_id;

    let (posts, next) = client_ref
        .db()
        .paginate_social_posts_rev(cursor, 20, move |post| post.author == author_id)
        .await;

    let posts = posts.into_iter().map(post_to_timeline_item).collect();

    let next_cursor = next.map(|c| TimelineCursorResponse {
        ts: c.ts.as_u64(),
        event_id: c.event_id.to_string(),
    });

    Ok(Json(TimelineResponse { posts, next_cursor }))
}

async fn get_single_post(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path((rostra_id, event_id_str)): Path<(RostraId, String)>,
) -> ApiResult<Json<TimelinePostItem>> {
    let event_id: ShortEventId = event_id_str
        .parse()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid event_id format"))?;

    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let post = client_ref
        .db()
        .get_social_post(event_id)
        .await
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Post not found"))?;

    if post.author != rostra_id {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Post not found for this author",
        ));
    }

    Ok(Json(post_to_timeline_item(post)))
}

// -- Timeline endpoints (Following / Network) --

#[derive(Deserialize)]
struct TimelineQuery {
    ts: Option<Timestamp>,
    event_id: Option<ShortEventId>,
}

#[derive(Serialize)]
struct TimelinePostItem {
    event_id: String,
    author: String,
    ts: u64,
    content: Option<String>,
    reply_to: Option<String>,
    persona_tags: Vec<String>,
    reply_count: u64,
}

#[derive(Serialize)]
struct TimelineResponse {
    posts: Vec<TimelinePostItem>,
    next_cursor: Option<TimelineCursorResponse>,
}

#[derive(Serialize)]
struct TimelineCursorResponse {
    ts: u64,
    event_id: String,
}

fn post_to_timeline_item(
    post: rostra_client_db::social::SocialPostRecord<SocialPost>,
) -> TimelinePostItem {
    let persona_tags = post
        .content
        .persona_tags()
        .into_iter()
        .map(|t| t.to_string())
        .collect();
    TimelinePostItem {
        event_id: post.event_id.to_string(),
        author: post.author.to_string(),
        ts: post.ts.as_u64(),
        content: post.content.djot_content,
        reply_to: post.reply_to.map(|r| r.to_string()),
        persona_tags,
        reply_count: post.reply_count,
    }
}

async fn get_following_timeline(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<Json<TimelineResponse>> {
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let cursor = query.ts.and_then(|ts| {
        query
            .event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
    });

    let self_id = rostra_id;
    let followees: HashMap<RostraId, PersonasTagsSelector> = client_ref
        .db()
        .get_followees(self_id)
        .await
        .into_iter()
        .collect();

    let (posts, next) = client_ref
        .db()
        .paginate_social_posts_rev(cursor, 20, move |post| {
            post.author != self_id
                && followees.get(&post.author).is_some_and(|selector| {
                    let tags = post.content.persona_tags();
                    if tags.is_empty() {
                        return true;
                    }
                    selector.matches_tags(&tags)
                })
        })
        .await;

    let posts = posts.into_iter().map(post_to_timeline_item).collect();

    let next_cursor = next.map(|c| TimelineCursorResponse {
        ts: c.ts.as_u64(),
        event_id: c.event_id.to_string(),
    });

    Ok(Json(TimelineResponse { posts, next_cursor }))
}

async fn get_network_timeline(
    State(state): State<SharedState>,
    _version: ApiVersion,
    Path(rostra_id): Path<RostraId>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<Json<TimelineResponse>> {
    state.load_client(rostra_id).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load client: {e}"),
        )
    })?;

    let client = state.client(rostra_id).await.map_err(|e| {
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

    let cursor = query.ts.and_then(|ts| {
        query
            .event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
    });

    let self_id = rostra_id;

    let (posts, next) = client_ref
        .db()
        .paginate_social_posts_rev(cursor, 20, move |post| post.author != self_id)
        .await;

    let posts = posts.into_iter().map(post_to_timeline_item).collect();

    let next_cursor = next.map(|c| TimelineCursorResponse {
        ts: c.ts.as_u64(),
        event_id: c.event_id.to_string(),
    });

    Ok(Json(TimelineResponse { posts, next_cursor }))
}
