use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup, PreEscaped};
use rostra_client_db::social::EventPaginationCursor;
use rostra_core::id::RostraId;
use tower_cookies::Cookies;

use super::timeline::{TimelineMode, TimelinePaginationInput};
use super::unlock::session::{RoMode, UserSession};
use super::Maud;
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

pub async fn post_follow(
    state: State<SharedState>,
    session: UserSession,
    Path(profile_id): Path<RostraId>,
) -> RequestResult<impl IntoResponse> {
    state
        .client(session.id())
        .await?
        .client_ref()?
        .follow(session.id_secret()?, profile_id)
        .await?;

    Ok(Maud(
        state
            .render_profile_summary(profile_id, &session, session.ro_mode())
            .await?,
    ))
}

pub async fn post_unfollow(
    state: State<SharedState>,
    session: UserSession,
    Path(profile_id): Path<RostraId>,
) -> RequestResult<impl IntoResponse> {
    state
        .client(session.id())
        .await?
        .client_ref()?
        .unfollow(session.id_secret()?, profile_id)
        .await?;

    Ok(Maud(
        state
            .render_profile_summary(profile_id, &session, session.ro_mode())
            .await?,
    ))
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

                    (self.new_post_form(None, session.ro_mode()))
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
                    "#
                    ))
                }
                img ."m-profileSummary__userImage u-userImage"
                    src=(self.avatar_url(profile_id))
                    width="32pt"
                    height="32pt"
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
                            @if following {
                                button
                                    ."m-profileSummary__unfollowButton u-button"
                                    hx-post=(format!("/ui/profile/{profile_id}/unfollow"))
                                    hx-target="closest .m-profileSummary"
                                    hx-swap="outerHTML"
                                    disabled[ro.to_disabled()]
                                {
                                    span ."m-profileSummary__unfollowButtonIcon u-buttonIcon" width="1rem" height="1rem"
                                    {}
                                    "Unfollow"
                                }
                            } @else {
                                button
                                    ."m-profileSummary__followButton u-button"
                                    hx-post=(format!("/ui/profile/{profile_id}/follow"))
                                    hx-target="closest .m-profileSummary"
                                    hx-swap="outerHTML"
                                    disabled[ro.to_disabled()]
                                {
                                    span ."m-profileSummary__followButtonIcon u-buttonIcon" width="1rem" height="1rem"
                                    {}
                                    "Follow"
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
