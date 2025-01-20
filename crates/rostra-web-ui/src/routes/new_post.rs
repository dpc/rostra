use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use serde::Deserialize;

use super::super::error::RequestResult;
use super::super::SharedAppState;
use super::Maud;
use crate::AppState;

#[derive(Deserialize)]
pub struct Input {
    content: String,
}

pub async fn new_post(
    state: State<SharedAppState>,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    state.client.client_ref()?.post(form.content).await?;
    Ok(Maud(html! {
        div {
            p { "Posted!" }
        }
        (state.new_post_form())
    }))
}

impl AppState {
    pub fn new_post_form(&self) -> Markup {
        html! {
            form ."m-newPostForm"
                hx-post="/post"
                hx-swap="outerHTML"
            {
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
