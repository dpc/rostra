use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use rostra_core::id::RostraId;
use serde::Deserialize;

use super::Maud;
use crate::error::RequestResult;
use crate::{AppState, SharedAppState};

#[derive(Deserialize)]
pub struct Input {
    rostra_id: RostraId,
}

pub async fn add_followee(
    state: State<SharedAppState>,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    state.client.client_ref()?.follow(form.rostra_id).await?;
    Ok(Maud(state.add_followee_form(html! {
    div {
        p { "Followed!" }
    }
    })))
}

impl AppState {
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
                    placeholder="RostraID"
                    type="text"
                    name="rostra_id"
                    {}
                button ".m-addFolloweeForm__submit" { "Follow" }
            }
        }
    }
}
