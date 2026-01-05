use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use rostra_core::id::RostraId;
use serde::{Deserialize, Serialize};

use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
pub struct ProfileSearchResult {
    rostra_id: String,
    display_name: String,
}

pub async fn search_profiles(
    state: State<SharedState>,
    session: UserSession,
    Query(params): Query<SearchQuery>,
) -> RequestResult<impl IntoResponse> {
    let query = params.q.to_lowercase();
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let db = client_ref.db();

    // Get extended followers (direct + followers of followers)
    let (direct, extended) = db.get_followees_extended(session.id()).await;

    // Collect all IDs to search (direct followees + extended)
    let all_ids: Vec<RostraId> = direct.keys().copied().chain(extended.into_iter()).collect();

    // Filter by display name prefix
    let mut results = Vec::new();
    for id in all_ids {
        if results.len() >= 10 {
            break;
        }

        if let Some(profile) = db.get_social_profile(id).await {
            if profile.display_name.to_lowercase().starts_with(&query) {
                results.push(ProfileSearchResult {
                    rostra_id: id.to_string(),
                    display_name: profile.display_name,
                });
            }
        }
    }

    Ok(Json(results))
}
