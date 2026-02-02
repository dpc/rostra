use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use maud::{Markup, html};
use rostra_core::id::RostraId;

use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::error::RequestResult;
use crate::{SharedState, UiState};

pub async fn get_settings() -> impl IntoResponse {
    Redirect::to("/ui/settings/followers")
}

pub async fn get_settings_followers(
    state: State<SharedState>,
    session: UserSession,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let user_id = client_ref.rostra_id();

    let followees = client_ref.db().get_followees(user_id).await;

    let navbar = state.render_settings_navbar(&session, "followers").await?;
    let content = state
        .render_followers_settings(&session, user_id, followees)
        .await?;

    Ok(Maud(
        state
            .render_settings_page(&session, navbar, content)
            .await?,
    ))
}

impl UiState {
    pub async fn render_settings_page(
        &self,
        _session: &UserSession,
        navbar: Markup,
        content: Markup,
    ) -> RequestResult<Markup> {
        self.render_html_page("Settings", self.render_page_layout(navbar, content))
            .await
    }

    pub async fn render_settings_navbar(
        &self,
        _session: &UserSession,
        active_category: &str,
    ) -> RequestResult<Markup> {
        Ok(html! {
            nav ."o-navBar" {
                div ."o-topNav" {
                    a ."o-topNav__item" href="/ui" {
                        span ."o-topNav__icon -back" {}
                        "Back"
                    }
                }

                div ."o-settingsNav" {
                    a ."o-settingsNav__item"
                        ."-active"[active_category == "followers"]
                        href="/ui/settings/followers"
                    {
                        "Followers"
                    }
                }
            }
        })
    }

    pub async fn render_followers_settings(
        &self,
        session: &UserSession,
        _user_id: RostraId,
        followees: Vec<(RostraId, rostra_core::event::PersonaSelector)>,
    ) -> RequestResult<Markup> {
        Ok(html! {
            div ."o-settingsContent" {
                h2 ."o-settingsContent__header" { "Followers" }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" { "Add Followee" }
                    (self.render_add_followee_form(None))
                }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" { "Following" }
                    (self.render_followee_list(session, followees).await?)
                }

                // Follow dialog container (shared by all followee items)
                div id="follow-dialog-content" {}
            }
        })
    }

    pub async fn render_followee_list(
        &self,
        session: &UserSession,
        followees: Vec<(RostraId, rostra_core::event::PersonaSelector)>,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        let mut followee_items = Vec::new();
        for (followee_id, _persona_selector) in followees {
            let profile = self.get_social_profile_opt(followee_id, &client_ref).await;
            let display_name = profile
                .as_ref()
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| followee_id.to_string());
            followee_items.push((followee_id, display_name));
        }

        // Sort by display name
        followee_items.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));

        Ok(html! {
            div id="followee-list" ."m-followeeList" {
                @if followee_items.is_empty() {
                    p ."o-settingsContent__empty" {
                        "You are not following anyone yet."
                    }
                } @else {
                    @for (followee_id, display_name) in &followee_items {
                        div ."m-followeeList__item" {
                            img ."m-followeeList__avatar u-userImage"
                                src=(self.avatar_url(*followee_id))
                                alt="Avatar"
                                width="32"
                                height="32"
                                loading="lazy"
                                {}
                            a ."m-followeeList__name"
                                href=(format!("/ui/profile/{}", followee_id))
                            {
                                (display_name)
                            }
                            (fragment::ajax_button(
                                &format!("/ui/profile/{followee_id}/follow"),
                                "get",
                                "follow-dialog-content",
                                "m-followeeList__followButton",
                                "Following...",
                            )
                            .disabled(session.ro_mode().to_disabled())
                            .hidden_inputs(html! { input type="hidden" name="following" value="true" {} })
                            .form_class("m-followeeList__actions")
                            .call())
                        }
                    }
                }
            }
        })
    }
}
