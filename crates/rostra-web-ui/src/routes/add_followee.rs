use axum::Form;
use axum::extract::State;
use axum::response::IntoResponse;
use maud::{Markup, html};
use rostra_core::event::PersonaSelector;
use rostra_core::id::RostraId;
use serde::Deserialize;

use super::Maud;
use super::unlock::session::UserSession;
use crate::error::RequestResult;
use crate::{SharedState, UiState};

#[derive(Deserialize)]
pub struct Input {
    rostra_id: RostraId,
}

pub async fn add_followee(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    state
        .client(session.id())
        .await?
        .client_ref()?
        .follow(
            session.id_secret()?,
            form.rostra_id,
            PersonaSelector::Except { ids: vec![] },
        )
        .await?;
    Ok(Maud(state.render_add_followee_form(html! {
        span { "Followed!" }
    })))
}

impl UiState {
    pub fn render_add_followee_form(&self, notification: impl Into<Option<Markup>>) -> Markup {
        let notification = notification.into();
        html! {
            form id="add-followee-form" ."m-addFolloweeForm"
                action="/ui/followee"
                method="post"
                x-target="add-followee-form"
                x-swap="outerHTML"
            {
                input ."m-addFolloweeForm__content"
                    placeholder="RostraId"
                    type="text"
                    name="rostra_id"
                    autocomplete="off"
                    {}

                div ."m-addFolloweeForm__bottomBar"{
                    div ."m-addFolloweeForm__notification"{
                        @if let Some(n) = notification {
                                (n)
                        }
                    }
                    button ."m-addFolloweeForm__followButton u-button" {
                        span ."m-addFolloweeForm__followButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                        "Follow"
                    }
                }
            }
        }
    }
}
