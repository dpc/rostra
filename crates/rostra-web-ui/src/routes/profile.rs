use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup, PreEscaped};
use rostra_client_db::social::EventPaginationCursor;
use rostra_core::id::RostraId;

use super::timeline::TimelinePaginationInput;
use super::unlock::session::{RoMode, UserSession};
use super::Maud;
use crate::error::RequestResult;
use crate::{SharedState, UiState};

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
    Path(user_id): Path<RostraId>,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
    });

    let navbar = html! {
        nav ."o-navBar" {
            div ."o-navBar__list" {
                span ."o-navBar__header" { "Rostra:" }
                a ."o-navBar__item" href="https://github.com/dpc/rostra/discussions" { "Support" }
                a ."o-navBar__item" href="https://github.com/dpc/rostra/wiki" { "Wiki" }
                a ."o-navBar__item" href="https://github.com/dpc/rostra" { "Github" }
            }

            div ."o-navBar__userAccount" {
                (state.render_profile_summary(user_id, &session, session.ro_mode()).await?)
            }

            (state.render_add_followee_form(None))

            (state.new_post_form(None, session.ro_mode()))
        }
    };

    Ok(Maud(
        state
            .render_timeline_page(navbar, pagination, &session, Some(user_id))
            .await?,
    ))
}

impl UiState {
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
                    span ."m-profileSummary__displayName" { (profile.display_name) }
                    div ."m-profileSummary__buttons" {
                        button
                            ."m-profileSummary__copyButton u-button"
                            data-value=(profile_id) onclick="copyIdToClipboard(event)"  {
                                span ."m-profileSummary__copyButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "RostraId"
                            }
                    }
                }
            }
        })
    }
}
