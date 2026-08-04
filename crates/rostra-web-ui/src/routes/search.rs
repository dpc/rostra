use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use rostra_core::id::{RostraId, ShortRostraId, ToShort as _};
use serde::{Deserialize, Serialize, Serializer};

use super::unlock::session::UserSession;
use crate::SharedState;
use crate::error::RequestResult;

/// Subsequence fuzzy match: each query char must appear in order in text.
/// Returns score > 0 on match, 0 on no match. Rewards consecutive matches
/// and matches at word boundaries (after _, -, space, or at start).
fn fuzzy_match(query: &str, text: &str) -> i32 {
    let q: Vec<char> = query.chars().collect();
    let t: Vec<char> = text.chars().collect();

    // Quick subsequence check
    let mut qi = 0;
    for &tc in &t {
        if qi < q.len() && tc == q[qi] {
            qi += 1;
        }
    }
    if qi < q.len() {
        return 0;
    }

    // Score the match
    let mut score: i32 = 0;
    qi = 0;
    let mut prev_match_idx: i32 = -2;
    for (ti, &tc) in t.iter().enumerate() {
        if qi < q.len() && tc == q[qi] {
            score += 1;
            if ti as i32 == prev_match_idx + 1 {
                score += 2;
            }
            if ti == 0 || t[ti - 1] == '_' || t[ti - 1] == '-' || t[ti - 1] == ' ' {
                score += 3;
            }
            prev_match_idx = ti as i32;
            qi += 1;
        }
    }

    score
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
pub struct ProfileSearchResult {
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
    let query = params.q.to_lowercase();
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let db = client_ref.db();

    // Get extended followers (direct + followers of followers)
    let (direct, extended) = db.get_followees_extended(session.id()).await;

    // Deduplicate IDs (direct followees can overlap with extended)
    let mut seen = std::collections::HashSet::new();
    let all_ids: Vec<_> = std::iter::once(session.id())
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

    Ok(Json(order_and_limit_results(scored)))
}

#[cfg(test)]
mod tests;
