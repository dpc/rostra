use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup, PreEscaped};
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

fn focus_on_new_post_content_input() -> Markup {
    html! {
        script {
            // focus on new post content input
            (PreEscaped(r#"
                (function() {
                    const content = document.querySelector('.m-newPostForm__content');
                    if (content != null) {
                        content.focus();
                        // on small devices, we want to keep the input in view,
                        // so we scroll to it; on larger ones this breaks scrolling preview
                        // into view
                        console.log(window.innerWidth);
                        if (window.innerWidth < 768) {
                            content.parentNode.scrollIntoView();
                        }
                    }
                })()
            "#))
        }
    }
}
fn scroll_preview_into_view() -> Markup {
    html! {
        script {
            (PreEscaped(r#"
                (function() {
                    const input = document.querySelector('.o-mainBarTimeline__item.-preview');
                    if (input != null) {
                        input.parentNode.scrollIntoView()
                    } else {
                        console.log("Not found preview?")
                    }
                })()
            "#))
        }
    }
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
                span { "Posted!" }
            }
        },
        session.ro_mode(),
    );
    let reply_to = if let Some(reply_to) = form.reply_to {
        Some((
            reply_to.rostra_id(),
            state
                .client(session.id())
                .await?
                .db()?
                .get_posts_by_id([reply_to.event_id()].into_iter())
                .await
                .get(&reply_to.event_id())
                .cloned(),
        ))
    } else {
        None
    };
    let reply_to = reply_to
        .as_ref()
        .map(|(rostra_id, record)| (*rostra_id, record.as_ref()));
    Ok(Maud(html! {

        (clean_form)

        // clean up the preview
        div ."o-mainBarTimeline__item -preview -empty"
            hx-swap-oob="outerHTML: .o-mainBarTimeline__item.-preview"
        { }

        // Insert new post at the top of the timeline, where the preview we just cleared was.
        div hx-swap-oob="afterend: .o-mainBarTimeline__item.-preview" {
            div ."o-mainBarTimeline__item"
                ."-reply"[reply_to.is_some()]
                ."-post"[reply_to.is_none()]
             {
                (state.post_overview(
                    &client_ref,
                    client_ref.rostra_id())
                    .maybe_reply_to(reply_to)
                    .event_id(event.event_id.to_short())
                    .content(&form.content)
                    .ro( session.ro_mode())
                    .call()
                .await?)
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
                ."-reply"[form.reply_to.is_some()]
                ."-post"[form.reply_to.is_none()]
            {
                (state.post_overview(
                    &client.client_ref()?,
                    self_id
                    )
                    .content(&form.content)
                    .ro(session.ro_mode())
                    .call().await?
                )
                (scroll_preview_into_view())
                (focus_on_new_post_content_input())
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
    session: UserSession,
    Query(form): Query<ReplyToInput>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    let display_name = if let Some(reply_to) = form.reply_to {
        client_ref
            .db()
            .get_social_profile(reply_to.rostra_id())
            .await
            .map(|p| p.display_name)
    } else {
        None
    };
    Ok(Maud(
        state.render_reply_to_line(form.reply_to, display_name),
    ))
}
impl UiState {
    fn render_reply_to_line(
        &self,
        reply_to: Option<ExternalEventId>,
        reply_to_display_name: Option<String>,
    ) -> Markup {
        html! {
            div ."m-newPostForm__replyToLine" {
                @if let Some(reply_to) = reply_to {
                    p ."m-newPostForm__replyToLabel" {
                        span ."m-newPostForm__replyToText" { "Reply to: " }
                        (reply_to_display_name.unwrap_or_else(
                            || reply_to.rostra_id().to_short().to_string()
                        ))
                    }

                input ."m-newPostForm__replyTo"
                    type="hidden"
                    name="reply_to"
                    autocomplete="off"
                    value=(reply_to)
                    readonly
                    {}
                }
                (focus_on_new_post_content_input())
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
                (self.render_reply_to_line(None, None))
                textarea
                    ."m-newPostForm__content"
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
                    div
                        ."m-newPostForm__emojiButton"
                    { "😀" }
                    button ."m-newPostForm__postButton u-button"
                        disabled[ro.to_disabled()]
                        type="submit"
                    {
                        span ."m-newPostForm__postButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                        "Post"
                    }
                }
                div
                    ."m-newPostForm__emojiBar -hidden"
                    role="tooltip" {
                    emoji-picker
                        data-source="/assets/libs/emoji-picker-element/data.json"
                    {}
                }

                script type="module" {
                    (PreEscaped(r#"
                        import { Picker } from '/assets/libs/emoji-picker-element/index.js';
                        import textFieldEdit from '/assets/libs/text-field-edit/index.js';

                        const button = document.querySelector('.m-newPostForm__emojiButton')
                        const tooltip = document.querySelector('.m-newPostForm__emojiBar')

                        button.onclick = () => {
                            tooltip.classList.toggle('-hidden')
                        }

                        document.querySelector('emoji-picker').addEventListener('emoji-click', e => {
                          textFieldEdit.insert(document.querySelector('.m-newPostForm__content'), e.detail.unicode);
                        })
                    "#));
                }

            }
            (submit_on_ctrl_enter(".m-newPostForm", ".m-newPostForm__content"))
        }
    }
}
