use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_client_db::social::EventPaginationCursor;
use rostra_core::event::PersonaId;
use rostra_core::id::RostraId;
use serde::Deserialize;
use tower_cookies::Cookies;

use super::Maud;
use super::timeline::{TimelineMode, TimelinePaginationInput};
use super::unlock::session::{RoMode, UserSession};
use crate::error::RequestResult;
use crate::{SharedState, UiState};

pub async fn get_profile(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    Path(profile_id): Path<RostraId>,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
    });

    Ok(Maud(
        state
            .render_timeline_page(
                state.render_navbar(profile_id, &session).await?,
                pagination,
                &session,
                &mut cookies,
                TimelineMode::Profile(profile_id),
            )
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct FollowQueryParams {
    following: bool,
}

pub async fn get_follow_dialog(
    state: State<SharedState>,
    session: UserSession,
    Path(profile_id): Path<RostraId>,
    Query(params): Query<FollowQueryParams>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let personas = client_ref.db().get_personas_for_id(profile_id).await;
    Ok(Maud(html! {
        div ."o-followDialog__content" {
            form ."o-followDialog__form"
                hx-post=(format!("/ui/profile/{}/follow", profile_id))
                hx-target=".m-profileSummary"
                hx-swap="outerHTML"
            {
                div ."o-followDialog__optionsContainer" {
                    div ."o-followDialog__selectContainer" {
                        select
                            name="follow_type"
                            id="follow-type-select"
                            ."o-followDialog__followTypeSelect"
                            onchange="togglePersonaList()"
                        {
                            option
                                value="unfollow"
                                selected[params.following]
                            { "Unfollow" }

                            option
                                value="follow_all"
                                selected[!params.following]
                            { "Follow All (except selected)" }

                            option
                                value="follow_only"
                            { "Follow Only (selected)" }
                        }
                    }

                    div ."o-followDialog__personaList" ."-visible"[!params.following] {
                        @for (persona_id, persona_display_name) in personas {
                            div ."o-followDialog__personaOption" {
                                input
                                    type="checkbox"
                                    id=(format!("persona_{}", persona_id))
                                    name="personas"
                                    value=(persona_id)
                                {}
                                label
                                    for=(format!("persona_{}", persona_id))
                                    ."o-followDialog__personaLabel"
                                { (persona_display_name) }
                            }
                        }
                    }
                }

                div ."o-followDialog__actions" {
                    button
                        ."o-followDialog__cancelButton u-button"
                        type="button"
                        onclick="document.querySelector('.o-followDialog').classList.remove('-active')"
                    {
                        span ."o-followDialog__cancelButtonIcon u-buttonIcon"
                            width="1rem" height="1rem" {}
                        "Back"
                    }

                    button ."o-followDialog__submitButton u-button" type="submit" {
                        span ."o-followDialog__submitButtonIcon u-buttonIcon"
                            width="1rem" height="1rem" {}
                        "Submit"
                    }
                }
            }
        }
    }))
}

#[derive(Deserialize)]
pub struct FollowFormData {
    follow_type: String,
    #[serde(default)]
    personas: Vec<PersonaId>,
}

pub async fn post_follow(
    state: State<SharedState>,
    session: UserSession,
    Path(profile_id): Path<RostraId>,
    axum_extra::extract::Form(form): axum_extra::extract::Form<FollowFormData>,
) -> RequestResult<impl IntoResponse> {
    match form.follow_type.as_str() {
        "unfollow" => {
            state
                .client(session.id())
                .await?
                .client_ref()?
                .unfollow(session.id_secret()?, profile_id)
                .await?;
        }
        "follow_all" | "follow_only" => {
            let ids = form.personas;
            state
                .client(session.id())
                .await?
                .client_ref()?
                .follow(
                    session.id_secret()?,
                    profile_id,
                    match form.follow_type.as_str() {
                        "follow_all" => rostra_core::event::PersonaSelector::Except { ids },
                        "follow_only" => rostra_core::event::PersonaSelector::Only { ids },
                        _ => unreachable!(),
                    },
                )
                .await?;
        }
        _ => {}
    }

    Ok(Maud(html! {
        // Update the profile summary
        (state
            .render_profile_summary(profile_id, &session, session.ro_mode())
            .await?)

        // Close the follow dialog
        div ."o-followDialog -empty"
            hx-swap-oob="outerHTML:.o-followDialog"
        {}
    }))
}

impl UiState {
    pub async fn render_navbar(
        &self,
        profile_id: RostraId,
        session: &UserSession,
    ) -> RequestResult<Markup> {
        Ok(html! {
                nav ."o-navBar" {
                    div ."o-navBar__list" {
                        span ."o-navBar__header" { "Rostra:" }
                        a ."o-navBar__item" href="https://github.com/dpc/rostra/discussions" { "Support" }
                        a ."o-navBar__item" href="https://github.com/dpc/rostra/wiki" { "Wiki" }
                        a ."o-navBar__item" href="https://github.com/dpc/rostra" { "Github" }
                    }

                    div ."o-navBar__userAccount" {
                        (self.render_profile_summary(profile_id, session, session.ro_mode()).await?)
                    }

                    (self.new_post_form(None, session.ro_mode(), None))
                }
        })
    }

    pub async fn render_profile_summary(
        &self,
        profile_id: RostraId,
        session: &UserSession,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let profile = self
            .get_social_profile(profile_id, &client.client_ref()?)
            .await;
        let following = client
            .db()?
            .get_followees(session.id())
            .await
            .iter()
            .any(|(id, _)| id == &profile_id);
        Ok(html! {
            div ."m-profileSummary" {
                script {
                    (PreEscaped(
                    r#"
                    function copyIdToClipboard(event) {
                        const target = event.target;
                        const id = target.getAttribute('data-value');
                        navigator.clipboard.writeText(id);
                        target.classList.add('-active');

                        setTimeout(() => {
                            target.classList.remove('-active');
                        }, 1000);
                    }
                    
                    function togglePersonaList() {
                        const selectedOption = document.querySelector('#follow-type-select').value;
                        const personaList = document.querySelector('.o-followDialog__personaList');
                        
                        if (selectedOption === 'follow_all' || selectedOption === 'follow_only') {
                            personaList.classList.add('-visible');
                        } else {
                            personaList.classList.remove('-visible');
                        }
                    }
                    
                    "#
                    ))
                }
                img ."m-profileSummary__userImage u-userImage"
                    src=(self.avatar_url(profile_id))
                    alt=(format!("{}'s avatar", profile.display_name))
                    width="32pt"
                    height="32pt"
                    loading="lazy"
                    { }

                div ."m-profileSummary__content" {
                    a ."m-profileSummary__displayName"
                        href=(format!("/ui/profile/{}", profile_id))
                    {
                        (profile.display_name)
                    }
                    div ."m-profileSummary__buttons" {
                        button
                            ."m-profileSummary__copyButton u-button"
                            data-value=(profile_id) onclick="copyIdToClipboard(event)"
                        {
                            span ."m-profileSummary__copyButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                            "RostraId"
                        }
                        @if session.id() != profile_id {
                            button
                                ."m-profileSummary__unfollowButton u-button"
                                hx-get=(format!("/ui/profile/{profile_id}/follow?following={following}"))
                                hx-target=".o-followDialog"
                                hx-swap="innerHTML"
                                hx-on::after-request="document.querySelector('.o-followDialog').classList.add('-active')"
                                disabled[ro.to_disabled()]
                            {
                                span ."m-profileSummary__followButtonIcon u-buttonIcon" width="1rem" height="1rem"
                                {}
                                @if following {
                                    "Following..."
                                } @else {
                                    "Follow..."
                                }
                            }
                        }
                    }
                    p ."m-profileSummary__bio" { (profile.bio) }
                }
            }

        })
    }
}
