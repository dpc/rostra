use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use serde::Deserialize;

use super::super::error::RequestResult;
use super::super::SharedState;
use super::Maud;
use crate::UiState;

#[derive(Deserialize)]
pub struct Input {
    content: String,
}

pub async fn new_post(
    state: State<SharedState>,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    state
        .client()
        .await?
        .client_ref()?
        .post(form.content)
        .await?;
    Ok(Maud(state.new_post_form(html! {
        div {
            p { "Posted!" }
        }
    })))
}

impl UiState {
    pub fn new_post_form(&self, notification: impl Into<Option<Markup>>) -> Markup {
        let notification = notification.into();
        html! {
            form ."m-newPostForm"
                hx-post="/ui/post"
                hx-swap="outerHTML"
            {
                @if let Some(n) = notification {
                    (n)
                }
                textarea ."m-newPostForm__content"
                    placeholder="What's on your mind?"
                    dir="auto"
                    name="content"
                    {}
                div ."m-newPostForm__footer"
                {
                    button ".m-newPostForm__submit" { "Post" }
                }
            }
        }
    }
}
