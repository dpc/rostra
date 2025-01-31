use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use rostra_core::id::ToShort as _;
use rostra_core::ExternalEventId;
use serde::Deserialize;

use super::super::error::RequestResult;
use super::super::SharedState;
use super::unlock::session::{RoMode, UserSession};
use super::Maud;
use crate::html_utils::{re_typeset_mathjax, submit_on_ctrl_enter};
use crate::UiState;

#[derive(Deserialize)]
pub struct Input {
    reply_to: Option<ExternalEventId>,
    content: String,
}

pub async fn post_new_post(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;
    let event = client_ref
        .social_post(session.id_secret()?, form.content.clone(), form.reply_to)
        .await?;

    let clean_form = state.new_post_form(
        html! {
            div {
                p { "Posted!" }
            }
        },
        session.ro_mode(),
    );
    Ok(Maud(html! {

        (clean_form)

        // clean up the preview
        div ."o-mainBarTimeline__item -preview -empty"
            hx-swap-oob="outerHTML: .o-mainBarTimeline__item.-preview"
        { }

        // Insert new post at the top of the timeline, where the preview we just cleared was.
        div hx-swap-oob="afterend: .o-mainBarTimeline__item.-preview" {
            div ."o-mainBarTimeline__item" {
                (state.post_overview(&client_ref, client_ref.rostra_id(), Some(event.event_id.to_short()), &form.content, session.ro_mode()).await?)
            }
        }
        (re_typeset_mathjax())

    }))
}

pub async fn get_post_preview(
    state: State<SharedState>,
    session: UserSession,
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

                (state.post_overview(&client.client_ref()?, self_id, None, &form.content, session.ro_mode()).await?)
                (re_typeset_mathjax())
            }
        } @else {
            div ."o-mainBarTimeline__item -preview -empty" { }
        }
    }))
}

#[derive(Deserialize)]
pub struct ReplyToInput {
    reply_to: Option<ExternalEventId>,
}

pub async fn get_reply_to(
    state: State<SharedState>,
    _session: UserSession,
    Query(form): Query<ReplyToInput>,
) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.render_reply_to_line(form.reply_to)))
}
impl UiState {
    fn render_reply_to_line(&self, reply_to: Option<ExternalEventId>) -> Markup {
        html! {
            div ."m-newPostForm__replyToLine" {
                @if let Some(reply_to) = reply_to {
                    p ."m-newPostForm__replyToLabel" {
                        span ."m-newPostForm__replyToText" { "Reply to: " }
                        (reply_to.rostra_id().to_short())
                    }

                input ."m-newPostForm__replyTo"
                    type="hidden"
                    name="reply_to"
                    autocomplete="off"
                    value=(reply_to)
                    readonly
                    {}
                }
            }
        }
    }

    pub fn new_post_form(&self, notification: impl Into<Option<Markup>>, ro: RoMode) -> Markup {
        let notification = notification.into();
        html! {
            form ."m-newPostForm"
                hx-post="/ui/post"
                hx-swap="outerHTML"
            {
                (self.render_reply_to_line(None))
                textarea ."m-newPostForm__content"
                    placeholder=(
                        if ro.to_disabled() {
                            "Read-Only mode."
                        } else {
                          "What's on your mind?"
                        })
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
                    disabled[ro.to_disabled()]
                    {}
                div ."m-newPostForm__footer" {
                    @if let Some(n) = notification {
                        (n)
                    }
                    a href="https://htmlpreview.github.io/?https://github.com/jgm/djot/blob/master/doc/syntax.html" target="_blank" { "Formatting" }
                    button ."m-newPostForm__postButton u-button"
                        disabled[ro.to_disabled()]
                        type="submit"
                    {
                        span ."m-newPostForm__postButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                        "Post"
                    }
                }
            }
            (submit_on_ctrl_enter(".m-newPostForm", ".m-newPostForm__content"))
        }
    }
}
