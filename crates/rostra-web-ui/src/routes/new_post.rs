use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup, PreEscaped};
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
        .social_post(form.content)
        .await?;

    let form = state.new_post_form(html! {
        div {
            p { "Posted!" }
        }
    });
    Ok(Maud(html! {

        (form)

        // clean up the preview
        div ."o-mainBarTimeline__item -preview -empty"
            hx-swap-oob="outerHTML: .o-mainBarTimeline__item.-preview"
        { }

    }))
}

pub async fn new_post_preview(
    state: State<SharedState>,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client().await?;
    let self_id = client.client_ref()?.rostra_id();
    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div ."o-mainBarTimeline__item -preview" {
                (state.post_overview(&client.client_ref()?, self_id, &form.content).await?)


                script {

                (PreEscaped(r#"
                    MathJax.typesetPromise();
                "#))
                }
            }
        } @else {
            div ."o-mainBarTimeline__item -preview -empty" { }
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
                    hx-target=".o-mainBarTimeline__item.-preview"
                    hx-swap="outerHTML"
                    hx-preserve="false"
                    autofocus
                    {}
                div ."m-newPostForm__footer" {
                    a href="https://htmlpreview.github.io/?https://github.com/jgm/djot/blob/master/doc/syntax.html" target="_blank" { "Formatting" }
                    button ".m-newPostForm__submit" type="submit" { "Post" }
                }
            }
            script {
                (PreEscaped(r#"
                    (function() {
                        const form = document.querySelector('.m-newPostForm');
                        const input = document.querySelector('.m-newPostForm__content');

                        input.addEventListener('keydown', (e) => {
                            if (e.ctrlKey && e.key === 'Enter') {
                                htmx.trigger(form, 'submit');
                            }
                        });
                    }())
                "#))
            }
        }
    }
}
