#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use axum::Form;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, html};
use rostra_client_db::social::EventPaginationCursor;
use rostra_core::event::PersonaTag;
use rostra_core::event::content_kind::PersonasTagsSelector;
use rostra_core::id::RostraId;
use serde::Deserialize;
use tower_cookies::Cookies;

use super::timeline::{TimelineCursor, TimelineMode, TimelinePaginationInput};
use super::unlock::session::{RoMode, UserSession};
use super::{Maud, fragment};
use crate::error::{ReadOnlyModeSnafu, RequestResult, UserRequestError};
use crate::layout::{OpenGraphMeta, truncate_at_word_boundary};
use crate::routes::url::{RostraPathId, profile_follow_url, profile_url, redirect_to_canonical};
use crate::util::extractors::AjaxRequest;
use crate::{SharedState, UiState};

#[allow(clippy::too_many_arguments)]
pub async fn get_profile(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    OriginalUri(original_uri): OriginalUri,
    Path(profile_id): Path<RostraPathId>,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| TimelineCursor::EventTime(EventPaginationCursor { ts, event_id }))
    });

    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let profile_id = profile_id.resolve(client_ref.db()).await.ok_or_else(|| {
        crate::error::RequestError::User {
            source: UserRequestError::SomethingNotFound,
        }
    })?;
    if let Some(response) = redirect_to_canonical(&original_uri, profile_url(profile_id)) {
        return Ok(response);
    }
    let profile = state.get_social_profile(profile_id, &client_ref).await;

    let profile_url = state.absolute_url(&profile_url(profile_id));
    let avatar_url = state.absolute_url(&state.avatar_url(profile_id, profile.event_id));
    let description = truncate_at_word_boundary(&profile.bio, 200);

    let og = OpenGraphMeta {
        title: profile.display_name.clone(),
        description: description.clone(),
        url: profile_url.clone(),
        image: Some(avatar_url.clone()),
    };

    let json_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "ProfilePage",
        "name": profile.display_name,
        "description": description,
        "url": profile_url,
        "mainEntity": {
            "@type": "Person",
            "name": profile.display_name,
            "description": description,
            "image": avatar_url,
        }
    })
    .to_string();

    Ok(Maud(
        state
            .render_timeline_page(
                state.render_navbar(profile_id, &session, true).await?,
                pagination,
                &session,
                &mut cookies,
                TimelineMode::Profile(profile_id),
                is_ajax,
                Some(&og),
                Some(&json_ld),
            )
            .await?,
    )
    .into_response())
}

#[derive(Deserialize)]
pub struct FollowQueryParams {
    following: bool,
}

pub async fn get_follow_dialog(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    OriginalUri(original_uri): OriginalUri,
    Path(profile_id): Path<RostraPathId>,
    Query(params): Query<FollowQueryParams>,
) -> RequestResult<Response> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let profile_id = profile_id.resolve(client_ref.db()).await.ok_or_else(|| {
        crate::error::RequestError::User {
            source: UserRequestError::SomethingNotFound,
        }
    })?;
    if let Some(response) = redirect_to_canonical(&original_uri, profile_follow_url(profile_id)) {
        return Ok(response);
    }
    let profile = state.get_social_profile(profile_id, &client_ref).await;
    let mut persona_tags = client_ref.db().get_persona_tags_for_id(profile_id).await;
    persona_tags.extend(PersonaTag::defaults());

    // Look up the current follow relationship to pre-populate the dialog
    let current_selector = if params.following {
        client_ref
            .db()
            .get_followees(session.id())
            .await
            .into_iter()
            .find(|(id, _)| *id == profile_id)
            .map(|(_, sel)| sel)
    } else {
        None
    };

    // Determine current follow type and selected tags
    let (follow_type, selected_tags) =
        prepare_follow_dialog_personas(current_selector.as_ref(), &mut persona_tags);
    let show_persona_list = follow_type != "unfollow";

    if is_ajax {
        let ajax_attrs = fragment::AjaxLoadingAttrs::for_class("o-followDialog__submitButton");
        return Ok(Maud(html! {
        div id="follow-dialog-content" ."o-followDialog -active" {
            (fragment::dialog_escape_handler("follow-dialog-content"))
            div ."o-followDialog__content" {
                h4 ."o-followDialog__title" {
                    "Following: "
                    (profile.display_name)
                }
                form ."o-followDialog__form"
                    action=(profile_follow_url(profile_id))
                    method="post"
                    x-target="profile-summary followee-list follower-list follow-dialog-content"
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
                            { "Unfollow" }

                            option
                                value="follow_all"
                                selected[follow_type == "follow_all"]
                            { "Follow All (except selected)" }

                            option
                                value="follow_only"
                                selected[follow_type == "follow_only"]
                            { "Follow Only (selected)" }
                        }
                    }

                    div ."o-followDialog__personaList" ."-visible"[show_persona_list] {
                        (fragment::persona_tag_select("personas")
                            .available_tags(&persona_tags)
                            .selected_tags(&selected_tags)
                            .id("follow-persona-tags")
                            .empty_label("none")
                            .call())
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
        })
        .into_response());
    }

    let back_url = profile_url(profile_id);
    let editor = html! {
        div ."o-mainBarTimeline__item" {
            div ."o-followDialog__content" {
                h1 ."o-followDialog__title" {
                    "Following: "
                    (profile.display_name)
                }
                form ."o-followDialog__form"
                    action=(profile_follow_url(profile_id))
                    method="post"
                {
                    div ."o-followDialog__optionsContainer" {
                        div ."o-followDialog__selectContainer" {
                            select
                                name="follow_type"
                                id="follow-type-select"
                                ."o-followDialog__followTypeSelect"
                                onchange="togglePersonaList()"
                            {
                                option value="unfollow" { "Unfollow" }
                                option
                                    value="follow_all"
                                    selected[follow_type == "follow_all"]
                                { "Follow All (except selected)" }
                                option
                                    value="follow_only"
                                    selected[follow_type == "follow_only"]
                                { "Follow Only (selected)" }
                            }
                        }

                        div ."o-followDialog__personaList" ."-visible"[show_persona_list] {
                            @for tag in &persona_tags {
                                label ."o-followDialog__personaOption" {
                                    input
                                        type="checkbox"
                                        name="personas"
                                        value=(tag.as_str())
                                        checked[selected_tags.contains(tag)]
                                    {}
                                    span ."o-followDialog__personaLabel" { (tag.as_str()) }
                                }
                            }
                            label ."o-followDialog__personaOption" {
                                span ."o-followDialog__personaLabel" { "Custom tag (optional)" }
                                input
                                    type="text"
                                    name="personas"
                                    maxlength="32"
                                    autocomplete="off"
                                {}
                            }
                        }
                    }

                    div ."o-followDialog__actions" {
                        a ."o-followDialog__cancelButton u-button" href=(back_url) { "Back" }
                        (fragment::button("o-followDialog__submitButton", "Submit").call())
                    }
                }
            }
        }
    };

    Ok(Maud(
        state
            .render_nojs_full_page(&session, "Edit Follow", editor)
            .await?,
    )
    .into_response())
}

fn prepare_follow_dialog_personas(
    current_selector: Option<&PersonasTagsSelector>,
    persona_tags: &mut BTreeSet<PersonaTag>,
) -> (&'static str, BTreeSet<PersonaTag>) {
    let (follow_type, selected_tags) = match current_selector {
        Some(PersonasTagsSelector::Except { ids }) => ("follow_all", ids.clone()),
        Some(PersonasTagsSelector::Only { ids }) => ("follow_only", ids.clone()),
        None => ("follow_all", BTreeSet::new()),
    };
    persona_tags.extend(selected_tags.iter().cloned());
    (follow_type, selected_tags)
}

#[derive(Deserialize)]
pub struct FollowFormData {
    follow_type: String,
    #[serde(default)]
    personas: Vec<String>,
}

pub async fn post_follow(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Path(profile_id): Path<RostraPathId>,
    axum_extra::extract::Form(form): axum_extra::extract::Form<FollowFormData>,
) -> RequestResult<Response> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let profile_id = profile_id.resolve(client_ref.db()).await.ok_or_else(|| {
        crate::error::RequestError::User {
            source: UserRequestError::SomethingNotFound,
        }
    })?;

    match form.follow_type.as_str() {
        "unfollow" => {
            client_ref.unfollow(id_secret, profile_id).await?;
        }
        "follow_all" | "follow_only" => {
            client_ref
                .follow(
                    id_secret,
                    profile_id,
                    follow_form_selector(&form.follow_type, &form.personas),
                )
                .await?;
        }
        _ => {}
    }

    if !is_ajax {
        return Ok(Redirect::to(&profile_url(profile_id)).into_response());
    }

    // Get updated lists for settings pages
    let followees = client_ref.db().get_followees(session.id()).await;
    let followers = client_ref.db().get_followers(session.id()).await;

    Ok(Maud(html! {
        // Update the profile summary (for profile page)
        (state
            .render_profile_summary(profile_id, &session, state.ro_mode(session.session_token()), true)
            .await?)

        // Update the followee list (for settings/following page)
        (state.render_followee_list(&session, followees).await?)

        // Update the follower list (for settings/followers page)
        (state.render_follower_list(&session, followers).await?)

        // Close the follow dialog by replacing with empty non-active version
        div id="follow-dialog-content" {}
    })
    .into_response())
}

fn follow_form_selector(follow_type: &str, personas: &[String]) -> PersonasTagsSelector {
    let ids = personas
        .iter()
        .filter_map(|tag| PersonaTag::new(tag).ok())
        .collect();
    match follow_type {
        "follow_all" => PersonasTagsSelector::Except { ids },
        "follow_only" => PersonasTagsSelector::Only { ids },
        _ => unreachable!(),
    }
}

impl UiState {
    /// Render the viewed user's navbar, with actions only on profile pages.
    pub async fn render_navbar(
        &self,
        profile_id: RostraId,
        session: &UserSession,
        show_profile_actions: bool,
    ) -> RequestResult<Markup> {
        let ro_mode = self.ro_mode(session.session_token());
        Ok(crate::layout::render_app_navbar(
            None,
            html! {
                div ."o-navBar__userAccount" {
                    (self.render_profile_summary(profile_id, session, ro_mode, show_profile_actions).await?)
                }
            },
        ))
    }

    /// Render profile content and optionally its profile-page actions.
    pub async fn render_profile_summary(
        &self,
        profile_id: RostraId,
        session: &UserSession,
        ro: RoMode,
        show_profile_actions: bool,
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
                (fragment::avatar("m-profileSummary__userImage", self.avatar_url(profile_id, profile.event_id), &format!("{}'s avatar", profile.display_name)))

                div ."m-profileSummary__content" {
                    div ."m-profileSummary__identity"[show_profile_actions] {
                        a ."m-profileSummary__displayName u-displayName"
                            href=(profile_url(profile_id))
                        {
                            (profile.display_name)
                        }
                        @if show_profile_actions {
                            (fragment::button("m-profileSummary__copyButton", "")
                                .button_type("button")
                                .data_value(&profile_id.to_string())
                                .onclick("copyIdToClipboard(event)")
                                .aria_label("Copy RostraId")
                                .title("Copy RostraId")
                                .call())
                        }
                    }
                }

                @if show_profile_actions && session.id() != profile_id {
                    div ."m-profileSummary__buttons" {
                        @let label = if following { "Following..." } else { "Follow..." };
                        (fragment::ajax_button(
                            &profile_follow_url(profile_id),
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
                        a ."m-profileSummary__messageButton u-button"
                            href=(super::messages::thread_url(profile_id))
                        {
                            span ."m-profileSummary__messageButtonIcon u-buttonIcon" aria-hidden="true" {}
                            "Message"
                        }
                    }
                }

                div ."m-profileSummary__bio" { (rendered_bio) }
            }

        })
    }
}
