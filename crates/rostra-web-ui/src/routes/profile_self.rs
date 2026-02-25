pub mod extractor;

use axum::extract::State;
use axum::response::IntoResponse;
use maud::{Markup, html};
use rostra_client::ClientRef;
use rostra_client_db::IdSocialProfileRecord;
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};

use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::error::{ReadOnlyModeSnafu, RequestResult};
use crate::{SharedState, UiState};

pub async fn post_self_account_edit(
    state: State<SharedState>,
    session: UserSession,
    form: extractor::InputForm,
) -> RequestResult<impl IntoResponse> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    let existing = state
        .client(session.id())
        .await?
        .client_ref()?
        .db()
        .get_social_profile(session.id())
        .await;

    state
        .client(session.id())
        .await?
        .client_ref()?
        .post_social_profile_update(
            id_secret,
            form.name,
            form.bio,
            form.avatar.or_else(|| existing.and_then(|e| e.avatar)),
        )
        .await?;

    Ok(Maud(state.render_self_profile_summary(&session).await?))
}

impl UiState {
    pub async fn get_social_profile(
        &self,
        id: RostraId,
        client: &ClientRef<'_>,
    ) -> IdSocialProfileRecord {
        client.db().get_social_profile(id).await.unwrap_or_else(|| {
            rostra_client_db::IdSocialProfileRecord {
                event_id: ShortEventId::ZERO,
                display_name: id.to_short().to_string(),
                bio: "".into(),
                avatar: None,
            }
        })
    }

    pub async fn get_social_profile_opt(
        &self,
        id: RostraId,
        client: &ClientRef<'_>,
    ) -> Option<IdSocialProfileRecord> {
        client.db().get_social_profile(id).await
    }

    pub fn avatar_url(&self, id: RostraId, event_id: ShortEventId) -> String {
        format!("/profile/{id}/avatar?v={event_id}")
    }

    pub async fn render_self_profile_summary(&self, user: &UserSession) -> RequestResult<Markup> {
        let client = self.client(user.id()).await?;
        let self_id = client.client_ref()?.rostra_id();
        let self_profile = self
            .get_social_profile(self_id, &client.client_ref()?)
            .await;
        Ok(html! {
            div id="self-profile-summary" ."m-profileSummary" {
                (fragment::avatar("m-profileSummary__userImage", self.avatar_url(self_id, self_profile.event_id), "Self avatar"))

                div ."m-profileSummary__content" {
                    a ."m-profileSummary__displayName u-displayName"
                        href=(format!("/profile/{}", self_id))
                    {
                        (self_profile.display_name)
                    }
                    div ."m-profileSummary__buttons" {
                        (fragment::button("m-profileSummary__copyButton", "RostraId")
                            .button_type("button")
                            .data_value(&self_id.to_string())
                            .onclick("copyIdToClipboard(event)")
                            .call())
                    }
                }
            }
        })
    }
}
