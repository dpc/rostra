use axum::Form;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_core::ExternalEventId;
use rostra_core::event::PersonaId;
use rostra_core::id::{RostraId, ToShort as _};
use serde::Deserialize;
use tower_cookies::Cookies;

use super::super::SharedState;
use super::super::error::RequestResult;
use super::Maud;
use super::cookies::CookiesExt as _;
use super::fragment;
use super::unlock::session::{RoMode, UserSession};
use crate::UiState;
use crate::html_utils::re_typeset_mathjax;

#[derive(Deserialize)]
pub struct PostInput {
    reply_to: Option<ExternalEventId>,
    content: String,
    persona: Option<u8>,
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
    mut cookies: Cookies,
    Form(form): Form<PostInput>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    // Save the selected persona in a cookie
    if let Some(persona_id) = form.persona {
        cookies.save_persona(client_ref.rostra_id(), persona_id);
    }

    let _event = client_ref
        .social_post(
            session.id_secret()?,
            form.content.clone(),
            form.reply_to,
            PersonaId(form.persona.unwrap_or_default()),
        )
        .await?;

    // Clear the form content after posting
    let clean_form = state.new_post_form(None, session.ro_mode(), Some(client_ref.rostra_id()));
    let reply_to = if let Some(reply_to) = form.reply_to {
        Some((
            reply_to.rostra_id(),
            reply_to.event_id(),
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
    let _reply_to = reply_to
        .as_ref()
        .map(|(rostra_id, event_id, record)| (*rostra_id, *event_id, record.as_ref()));

    Ok(Maud(html! {
        // new clean form (this is the main target)
        (clean_form)

        // Close the preview dialog (x-sync will update this)
        div id="preview-dialog" ."o-previewDialog" {}

        // Clear the inline preview (x-sync will update this)
        div id="post-preview" ."o-mainBarTimeline__item -preview -empty" { }

        // Show success notification
        div id="ajax-scripts" {
            script {
                (PreEscaped(r#"
                    window.dispatchEvent(new CustomEvent('notify', {
                        detail: { type: 'success', message: 'Post published successfully' }
                    }));
                "#))
            }
        }

        (re_typeset_mathjax())
    }))
}

pub async fn get_post_preview_dialog(
    state: State<SharedState>,
    session: UserSession,
    cookies: Cookies,
    Form(form): Form<PostInput>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let self_id = client_ref.rostra_id();

    if form.content.is_empty() {
        return Ok(Maud(html! {
            div id="preview-dialog" ."o-previewDialog" {}
        }));
    }
    let personas = client_ref.db().get_personas_for_id(self_id).await;

    // Get the saved persona from cookie
    let saved_persona = cookies.get_persona(self_id);

    Ok(Maud(html! {
        div id="preview-dialog" ."o-previewDialog -active" {
            div ."o-previewDialog__content" {
                div ."o-previewDialog__post" {
                    (state.render_post_context(
                        &client.client_ref()?,
                        self_id
                        )
                        .content(&form.content)
                        .timestamp(rostra_core::Timestamp::now())
                        .ro(session.ro_mode())
                        .call().await?
                    )
                }

                @let ajax_attrs = fragment::AjaxLoadingAttrs::for_class("o-previewDialog__submitButton");
                div ."o-previewDialog__actions" {
                    form ."o-previewDialog__form"
                        action="/ui/post"
                        method="post"
                        x-target="new-post-form preview-dialog post-preview ajax-scripts"
                        "x-on:keyup.enter.ctrl.shift"="$el.requestSubmit()"
                        "@ajax:before"=(ajax_attrs.before)
                        "@ajax:after"=(ajax_attrs.after)
                    {
                        input type="hidden" name="content" value=(form.content) {}
                        @if let Some(reply_to) = form.reply_to {
                            input type="hidden" name="reply_to" value=(reply_to) {}
                        }

                        div ."o-previewDialog__actionContainer" {
                            div ."o-previewDialog__personaContainer" {
                                select
                                    name="persona"
                                    id="persona-select"
                                    ."o-previewDialog__personaSelect"
                                    x-autofocus
                                {
                                    @for (persona_id, persona_display_name) in personas {
                                        option
                                            value=(persona_id)
                                            selected[saved_persona.map_or(false, |id| PersonaId(id) == persona_id)]
                                        { (persona_display_name) }
                                    }
                                }
                            }

                            div ."o-previewDialog__actionButtons" {
                                (fragment::button("o-previewDialog__cancelButton", "Cancel")
                                    .button_type("button")
                                    .onclick("document.querySelector('.o-previewDialog').classList.remove('-active')")
                                    .call())

                                (fragment::button("o-previewDialog__submitButton", "Post").call())
                            }
                        }
                    }
                }
            }
            (re_typeset_mathjax())
        }
    }))
}

pub async fn get_post_preview(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<PostInput>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let self_id = client.client_ref()?.rostra_id();
    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div id="post-preview" ."o-mainBarTimeline__item -preview"
                ."-reply"[form.reply_to.is_some()]
                ."-post"[form.reply_to.is_none()]
            {
                (state.render_post_context(
                    &client.client_ref()?,
                    self_id
                    )
                    .content(&form.content)
                    .timestamp(rostra_core::Timestamp::now())
                    .ro(session.ro_mode())
                    .call().await?
                )
                (scroll_preview_into_view())
                (focus_on_new_post_content_input())
                (re_typeset_mathjax())
            }
        } @else {
            div id="post-preview" ."o-mainBarTimeline__item -preview -empty" { }
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
            div id="reply-to-line" ."m-newPostForm__replyToLine" {
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

    pub fn new_post_form(
        &self,
        _notification: impl Into<Option<Markup>>,
        ro: RoMode,
        user_id: Option<RostraId>,
    ) -> Markup {
        html! {
            // Hidden form for inline preview updates (must be outside main form)
            form id="inline-preview-form"
                action="/ui/post/preview"
                method="post"
                style="display: none;"
                x-target="post-preview"
                x-autofocus
            {
                input type="hidden" name="content" value="" {}
            }

            @let form_ajax = fragment::AjaxLoadingAttrs::for_class("m-newPostForm__previewButton");
            form id="new-post-form" ."m-newPostForm"
                action="/ui/post/preview_dialog"
                method="post"
                x-target="preview-dialog"
                "@ajax:before"=(form_ajax.before)
                "@ajax:after"=(form_ajax.after)
            {
                (self.render_reply_to_line(None, None))
                div ."m-newPostForm__textareaWrapper"
                    x-data="mentionAutocomplete"
                    style="position: relative;"
                {
                    textarea
                        ."m-newPostForm__content"
                        placeholder=(
                            if ro.to_disabled() {
                                "Read-only view. Logout to change."
                            } else {
                              "What's on your mind?"
                            })
                        dir="auto"
                        name="content"
                        "@input"=r#"
                            handleMentionInput($event);
                            // Also handle preview update
                            const previewForm = document.getElementById('inline-preview-form');
                            const contentInput = previewForm.querySelector('input[name=content]');
                            contentInput.value = $el.value;
                            previewForm.requestSubmit();
                        "#
                        "@keydown"="handleKeydown($event)"
                        autocomplete="off"
                        autofocus
                        disabled[ro.to_disabled()]
                        "x-on:keyup.enter.ctrl"="$el.form.requestSubmit()"
                        {}

                    // Autocomplete dropdown
                    div ."m-mentionAutocomplete"
                        x-show="showDropdown"
                        x-cloak
                        "@click.outside"="showDropdown = false"
                    {
                        template x-for="(result, index) in results" ":key"="result.rostra_id" {
                            div ."m-mentionAutocomplete__item"
                                ":class"="{ '-selected': index === selectedIndex }"
                                "@click"="selectProfile(result)"
                            {
                                span ."m-mentionAutocomplete__displayName" x-text="result.display_name" {}
                                span ."m-mentionAutocomplete__id" x-text="'@' + result.rostra_id.substring(0, 8)" {}
                            }
                        }
                        div x-show="results.length === 0 && query.length > 0" ."m-mentionAutocomplete__empty" {
                            "No matches found"
                        }
                    }
                }

                div ."m-newPostForm__footer" {
                    div ."m-newPostForm__footerRow m-newPostForm__footerRow--main" {
                        a href="https://htmlpreview.github.io/?https://github.com/jgm/djot/blob/master/doc/syntax.html" target="_blank" { "Formatting" }
                        a
                            ."m-newPostForm__emojiButton"
                            href="#"
                            onclick=r#"
                                event.preventDefault();
                                const tooltip = document.querySelector('.m-newPostForm__emojiBar');
                                if (tooltip) {
                                    tooltip.classList.toggle('-hidden');
                                    if (!tooltip.classList.contains('-hidden')) {
                                        const emojiPicker = document.querySelector('emoji-picker');
                                        if (emojiPicker && emojiPicker.shadowRoot) {
                                            const searchInput = emojiPicker.shadowRoot.querySelector('#search');
                                            if (searchInput) searchInput.focus();
                                        }
                                    }
                                }
                            "#
                        { "😀" }
                        (fragment::button("m-newPostForm__previewButton", "Preview")
                            .disabled(ro.to_disabled())
                            .call())
                    }
                    div ."m-newPostForm__footerRow m-newPostForm__footerRow--media" {
                        @if user_id.is_some() {
                            button ."m-newPostForm__attachButton u-button"
                                ."-disabled"[ro.to_disabled()]
                                type="submit"
                                form="media-attach-form"
                            {
                                span ."m-newPostForm__attachButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Attach"
                            }
                        } @else {
                            button ."m-newPostForm__attachButton u-button"
                                disabled
                                type="button"
                            {
                                span ."m-newPostForm__attachButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Attach"
                            }
                        }
                        @if user_id.is_some() {
                            button ."m-newPostForm__uploadButton u-button"
                                disabled[ro.to_disabled()]
                                type="button"
                                onclick="document.getElementById('media-file-input').click()"
                            {
                                span ."m-newPostForm__uploadButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Upload"
                            }
                        } @else {
                            button ."m-newPostForm__uploadButton u-button"
                                disabled
                                type="button"
                            {
                                span ."m-newPostForm__uploadButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Upload"
                            }
                        }
                    }
                }
            }

            // Separate forms for media operations (outside main form to avoid nesting)
            @if let Some(uid) = user_id {
                @let attach_ajax = fragment::AjaxLoadingAttrs::for_document_class("m-newPostForm__attachButton");
                form id="media-attach-form"
                    action=(format!("/ui/media/{}/list", uid))
                    method="get"
                    x-target="media-list"
                    style="display: none;"
                    "@ajax:before"=(attach_ajax.before)
                    "@ajax:after"=(attach_ajax.after)
                {}

                @let upload_ajax = fragment::AjaxLoadingAttrs::for_document_class("m-newPostForm__uploadButton");
                form id="media-upload-form"
                    action="/ui/media/publish"
                    method="post"
                    enctype="multipart/form-data"
                    x-target="ajax-scripts"
                    style="display: none;"
                    "@ajax:before"=(upload_ajax.before)
                    "@ajax:after"=(upload_ajax.after)
                {
                    input id="media-file-input"
                        name="media_file"
                        type="file"
                        accept="image/*,video/*,audio/*"
                        "@change"="$el.form.requestSubmit(); $el.value = '';"
                        {}
                }
            }

            // Emoji picker (outside form to avoid re-creation on swap)
            div id="emoji-picker-container" ."m-newPostForm__emojiBar -hidden"
                role="tooltip" {
                emoji-picker
                    data-source="/assets/libs/emoji-picker-element/data.json"
                {}
            }
        }
    }
}
