use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use serde::Deserialize;

use super::super::error::RequestResult;
use super::super::SharedState;
use super::Maud;
use crate::fragment::post_overview;
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

pub async fn new_post_preview(
    state: State<SharedState>,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    let self_id = state.client().await?.client_ref()?.rostra_id();
    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div ."o-mainBarTimeline__item o-mainBarTimeline__preview" {
                (post_overview(&self_id.to_string(), &form.content))
            }
        } else {
            div ."o-mainBarTimeline__preview" { }
        }
    }))
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
                    hx-post="/ui/post/preview"
                    hx-include="closest form"
                    hx-trigger="input changed delay:.2s"
                    hx-target=".o-mainBarTimeline__preview"
                    hx-swap="outerHTML"
                    {}
                div ."m-newPostForm__footer" {
                    a href="https://www.djot.net/playground/" target="_blank" { "Formatting" }
                    button ".m-newPostForm__submit" type="submit" { "Post" }
                }
            }
        }
    }
}
