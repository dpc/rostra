use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use rostra_core::id::RostraId;
use serde::Deserialize;

use super::Maud;
use crate::error::RequestResult;
use crate::{SharedState, UiState};

#[derive(Deserialize)]
pub struct Input {
    rostra_id: RostraId,
}

pub async fn add_followee(
    state: State<SharedState>,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    state
        .client()
        .await?
        .client_ref()?
        .follow(form.rostra_id)
        .await?;
    Ok(Maud(state.add_followee_form(html! {
    div {
        p { "Followed!" }
    }
    })))
}

impl UiState {
    pub fn add_followee_form(&self, notification: impl Into<Option<Markup>>) -> Markup {
        let notification = notification.into();
        html! {
            form ."m-addFolloweeForm"
                hx-post="/ui/followee"
                hx-swap="outerHTML"
            {
                @if let Some(n) = notification {
                    (n)
                }
                input ."m-addFolloweeForm__content"
                    placeholder="RostraId"
                    type="text"
                    name="rostra_id"
                    autocomplete="off"
                    {}
                button ".m-addFolloweeForm__submit" { "Follow" }
            }
        }
    }
}
