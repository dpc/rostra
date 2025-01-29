use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use serde::Deserialize;

use super::super::error::RequestResult;
use super::super::SharedState;
use super::unlock::session::AuthenticatedUser;
use super::Maud;
use crate::html_utils::{re_typeset_mathjax, submit_on_ctrl_enter};
use crate::UiState;

#[derive(Deserialize)]
pub struct Input {
    content: String,
}

pub async fn post_new_post(
    state: State<SharedState>,
    session: AuthenticatedUser,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;
    client_ref.social_post(form.content.clone()).await?;

    let clean_form = state.new_post_form(html! {
        div {
            p { "Posted!" }
        }
    });
    Ok(Maud(html! {

        (clean_form)

        // clean up the preview
        div ."o-mainBarTimeline__item -preview -empty"
            hx-swap-oob="outerHTML: .o-mainBarTimeline__item.-preview"
        { }

        // Insert new post at the top of the timeline, where the preview we just cleared was.
        div hx-swap-oob="afterend: .o-mainBarTimeline__item.-preview" {
            div ."o-mainBarTimeline__item" {
                (state.post_overview(&client_ref, client_ref.rostra_id(), &form.content).await?)
            }
        }
        (re_typeset_mathjax())

    }))
}

pub async fn get_post_preview(
    state: State<SharedState>,
    session: AuthenticatedUser,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let self_id = client.client_ref()?.rostra_id();
    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div ."o-mainBarTimeline__item -preview"
                // We want to show the preview, even if the user was scrolling, and to scroll
                // all the way to the top, we actually want the parent of a parent.
                "hx-on::load"="this.parentNode.parentNode.scrollIntoView()" {

                (state.post_overview(&client.client_ref()?, self_id, &form.content).await?)
                (re_typeset_mathjax())
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
                    autocomplete="off"
                    autofocus
                    {}
                div ."m-newPostForm__footer" {
                    @if let Some(n) = notification {
                        (n)
                    }
                    a href="https://htmlpreview.github.io/?https://github.com/jgm/djot/blob/master/doc/syntax.html" target="_blank" { "Formatting" }
                    button ".m-newPostForm__submit" type="submit" { "Post" }
                }
            }
            (submit_on_ctrl_enter(".m-newPostForm", ".m-newPostForm__content"))
        }
    }
}
