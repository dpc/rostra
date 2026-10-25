use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use rostra_client_db::Database;
use rostra_client_db::known_people::fuzzy_match;
use rostra_core::id::{RostraId, ShortRostraId, ToShort as _};
use serde::{Deserialize, Serialize, Serializer};

use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
pub(crate) struct ProfileSearchResult {
    rostra_id_reference: ProfileSearchIdReference,
    display_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileSearchIdReference {
    Full(RostraId),
    Short(ShortRostraId),
}

impl ProfileSearchIdReference {
    fn for_known_identity(id: RostraId, known_identity: Option<RostraId>) -> Self {
        if known_identity == Some(id) {
            Self::Short(id.to_short())
        } else {
            Self::Full(id)
        }
    }
}

impl Serialize for ProfileSearchIdReference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Full(id) => serializer.collect_str(id),
            Self::Short(id) => serializer.collect_str(id),
        }
    }
}

fn order_and_limit_results(
    mut scored: Vec<(i32, RostraId, ProfileSearchResult)>,
) -> Vec<ProfileSearchResult> {
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(10)
        .map(|(_, _, result)| result)
        .collect()
}

pub async fn search_profiles(
    state: State<SharedState>,
    session: UserSession,
    Query(params): Query<SearchQuery>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    Ok(Json(
        profile_search_results(client_ref.db(), session.id(), &params.q).await,
    ))
}

/// Find mention candidates without prescribing the request transport.
pub(crate) async fn profile_search_results(
    db: &Database,
    self_id: RostraId,
    query: &str,
) -> Vec<ProfileSearchResult> {
    let query = query.to_lowercase();
    // Get extended followers (direct + followers of followers)
    let (direct, extended) = db.get_followees_extended(self_id).await;

    // Deduplicate IDs (direct followees can overlap with extended)
    let mut seen = std::collections::HashSet::new();
    let all_ids: Vec<_> = std::iter::once(self_id)
        .chain(direct.keys().copied())
        .chain(extended)
        .filter(|id| seen.insert(*id))
        .collect();

    // Fuzzy match against display name or rostra_id
    let mut scored = Vec::new();
    for id in all_ids {
        let id_str = id.to_string();

        // Get display name from profile, or use truncated rostra_id as fallback
        let display_name = db
            .get_social_profile(id)
            .await
            .map(|p| p.display_name)
            .unwrap_or_else(|| format!("{}...", &id_str[..8.min(id_str.len())]));

        let name_score = fuzzy_match(&query, &display_name.to_lowercase());
        let id_score = fuzzy_match(&query, &id_str.to_lowercase());
        let score = name_score.max(id_score);

        if 0 < score {
            let rostra_id_reference = ProfileSearchIdReference::for_known_identity(
                id,
                db.get_known_identity(id.to_short()).await,
            );
            scored.push((
                score,
                id,
                ProfileSearchResult {
                    rostra_id_reference,
                    display_name,
                },
            ));
        }
    }

    order_and_limit_results(scored)
}

#[cfg(test)]
mod tests;
