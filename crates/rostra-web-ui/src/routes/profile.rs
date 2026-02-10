use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_client_db::social::EventPaginationCursor;
use rostra_core::event::PersonaId;
use rostra_core::id::RostraId;
use serde::Deserialize;
use tower_cookies::Cookies;

use super::timeline::{TimelineCursor, TimelineMode, TimelinePaginationInput};
use super::unlock::session::{RoMode, UserSession};
use super::{Maud, fragment};
use crate::error::{ReadOnlyModeSnafu, RequestResult};
use crate::util::extractors::AjaxRequest;
use crate::{SharedState, UiState};

pub async fn get_profile(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Path(profile_id): Path<RostraId>,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| TimelineCursor::ByEventTime(EventPaginationCursor { ts, event_id }))
    });

    Ok(Maud(
        state
            .render_timeline_page(
                state.render_navbar(profile_id, &session).await?,
                pagination,
                &session,
                &mut cookies,
                TimelineMode::Profile(profile_id),
                is_ajax,
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
    let profile = state.get_social_profile(profile_id, &client_ref).await;
    let personas = client_ref.db().get_personas_for_id(profile_id).await;
    let ajax_attrs = fragment::AjaxLoadingAttrs::for_class("o-followDialog__submitButton");
    Ok(Maud(html! {
        div id="follow-dialog-content" ."o-followDialog -active" {
            script {
                (PreEscaped(r#"
                function togglePersonaList() {
                    const selectedOption = document.querySelector('#follow-type-select').value;
                    const personaList = document.querySelector('.o-followDialog__personaList');

                    if (selectedOption === 'follow_all' || selectedOption === 'follow_only') {
                        personaList.classList.add('-visible');
                    } else {
                        personaList.classList.remove('-visible');
                    }
                }
                "#))
            }
            (fragment::dialog_escape_handler("follow-dialog-content"))
            div ."o-followDialog__content" {
                h4 ."o-followDialog__title" {
                    "Following "
                    (profile.display_name)
                }
                form ."o-followDialog__form"
                    action=(format!("/profile/{}/follow", profile_id))
                    method="post"
                    x-target="profile-summary followee-list follow-dialog-content"
                    "@ajax:before"=(ajax_attrs.before)
                    "@ajax:after"=(ajax_attrs.after)
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
                    (fragment::button("o-followDialog__cancelButton", "Back")
                        .button_type("button")
                        .onclick("document.querySelector('#follow-dialog-content').classList.remove('-active')")
                        .call())

                    (fragment::button("o-followDialog__submitButton", "Submit").call())
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
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;

    match form.follow_type.as_str() {
        "unfollow" => {
            client_ref.unfollow(id_secret, profile_id).await?;
        }
        "follow_all" | "follow_only" => {
            let ids = form.personas;
            client_ref
                .follow(
                    id_secret,
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

    // Get updated followees list for settings page
    let followees = client_ref.db().get_followees(session.id()).await;

    Ok(Maud(html! {
        // Update the profile summary (for profile page)
        (state
            .render_profile_summary(profile_id, &session, state.ro_mode(session.session_token()))
            .await?)

        // Update the followee list (for settings page)
        (state.render_followee_list(&session, followees).await?)

        // Close the follow dialog by replacing with empty non-active version
        div id="follow-dialog-content" {}
    }))
}

impl UiState {
    pub async fn render_navbar(
        &self,
        profile_id: RostraId,
        session: &UserSession,
    ) -> RequestResult<Markup> {
        let ro_mode = self.ro_mode(session.session_token());
        Ok(html! {
                nav ."o-navBar" {
                    (self.render_top_nav())

                    div ."o-navBar__userAccount" {
                        (self.render_profile_summary(profile_id, session, ro_mode).await?)
                    }

                    (self.new_post_form(None, ro_mode, Some(session.id())))
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
        let client_ref = client.client_ref()?;
        let profile = self.get_social_profile(profile_id, &client_ref).await;
        let following = client
            .db()?
            .get_followees(session.id())
            .await
            .iter()
            .any(|(id, _)| id == &profile_id);
        let rendered_bio = self.render_bio(client_ref, &profile.bio).await;
        Ok(html! {
            div id="profile-summary" ."m-profileSummary" {
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
                    a ."m-profileSummary__displayName u-displayName"
                        href=(format!("/profile/{}", profile_id))
                    {
                        (profile.display_name)
                    }
                    div ."m-profileSummary__buttons" {
                        (fragment::button("m-profileSummary__copyButton", "RostraId")
                            .button_type("button")
                            .data_value(&profile_id.to_string())
                            .onclick("copyIdToClipboard(event)")
                            .call())
                        @if session.id() != profile_id {
                            @let label = if following { "Following..." } else { "Follow..." };
                            (fragment::ajax_button(
                                &format!("/profile/{profile_id}/follow"),
                                "get",
                                "follow-dialog-content",
                                "m-profileSummary__followButton",
                                label,
                            )
                            .disabled(ro.to_disabled())
                            .hidden_inputs(html! {
                                input type="hidden" name="following" value=(following);
                            })
                            .call())
                        }
                    }
                }

                div ."m-profileSummary__bio" { (rendered_bio) }
            }

        })
    }
}
