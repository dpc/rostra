use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
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

    // Filter by display name or rostra_id prefix (self + direct followees +
    // extended)
    let mut results = Vec::new();
    for id in std::iter::once(session.id())
        .chain(direct.keys().copied())
        .chain(extended)
    {
        if results.len() >= 10 {
            break;
        }

        let id_str = id.to_string();
        let id_str_lower = id_str.to_lowercase();

        // Get display name from profile, or use truncated rostra_id as fallback
        let display_name = db
            .get_social_profile(id)
            .await
            .map(|p| p.display_name)
            .unwrap_or_else(|| format!("{}...", &id_str[..8.min(id_str.len())]));

        let display_name_lower = display_name.to_lowercase();

        // Match against display name or rostra_id prefix
        if display_name_lower.starts_with(&query) || id_str_lower.starts_with(&query) {
            results.push(ProfileSearchResult {
                rostra_id: id_str,
                display_name,
            });
        }
    }

    Ok(Json(results))
}
