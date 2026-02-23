use axum::Form;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_core::event::PersonaId;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};
use serde::Deserialize;
use tower_cookies::Cookies;

use super::super::SharedState;
use super::super::error::{ReadOnlyModeSnafu, RequestResult};
use super::cookies::CookiesExt as _;
use super::post::{
    post_inline_reply_added_html_id, post_inline_reply_form_html_id,
    post_inline_reply_preview_html_id,
};
use super::unlock::session::{RoMode, UserSession};
use super::{Maud, fragment};
use crate::UiState;
use crate::html_utils::re_typeset;
use crate::util::extractors::ajax_request::AjaxRequest;

#[derive(Deserialize)]
pub struct PostInput {
    reply_to: Option<ExternalEventId>,
    content: String,
    persona: Option<u8>,
    /// For inline reply mode: the post thread context ID
    post_thread_id: Option<ShortEventId>,
    /// Where to redirect after posting (no-JS fallback)
    redirect: Option<String>,
}

fn focus_on_new_post_content_input() -> Markup {
    html! {
        script {
            // focus on new post content input
            (PreEscaped(r#"
                (function() {
                    const content = document.getElementById('new-post-content');
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
fn scroll_new_post_preview_into_view() -> Markup {
    html! {
        script {
            (PreEscaped(r#"
                (function() {
                    const input = document.getElementById('new-post-preview');
                    if (input != null) {
                        input.parentNode.scrollIntoView()
                    } else {
                        console.log("Not found new-post-preview?")
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
    AjaxRequest(is_ajax): AjaxRequest,
    Form(form): Form<PostInput>,
) -> RequestResult<impl IntoResponse> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    // Save the selected persona in a cookie
    if let Some(persona_id) = form.persona {
        cookies.save_persona(client_ref.rostra_id(), persona_id);
    }

    let redirect_to = form.redirect.clone();

    let event = client_ref
        .social_post(
            id_secret,
            form.content.clone(),
            form.reply_to,
            PersonaId(form.persona.unwrap_or_default()),
        )
        .await?;

    // No-JS: redirect back to the page the user was on
    if !is_ajax {
        let location = redirect_to.as_deref().unwrap_or("/");
        return Ok(Maud(html! {
            (maud::DOCTYPE)
            html {
                head {
                    meta http-equiv="refresh" content=(format!("0;url={location}")) {}
                }
                body {
                    p { "Post published. Redirecting..." }
                    a href=(location) { "Click here if not redirected." }
                }
            }
        }));
    }

    // If this is an inline reply, insert the new reply after the added placeholder
    if let (Some(post_thread_id), Some(reply_to)) = (form.post_thread_id, form.reply_to) {
        let reply_to_id = reply_to.event_id().to_short();
        let form_id = post_inline_reply_form_html_id(post_thread_id, reply_to_id);
        let preview_id = post_inline_reply_preview_html_id(post_thread_id, reply_to_id);
        let added_id = post_inline_reply_added_html_id(post_thread_id, reply_to_id);
        let self_id = client_ref.rostra_id();
        let event_id = event.event_id.to_short();

        return Ok(Maud(html! {
            // Clear the form placeholder
            div id=(form_id) {}

            // Clear the preview placeholder
            div id=(preview_id) {}

            // Insert new reply after the added placeholder (x-merge="after" on target)
            div id=(added_id) {
                div ."o-postOverview__commentsItem" {
                    (state.render_post_context(
                        &client_ref,
                        self_id
                        )
                        .event_id(event_id)
                        .post_thread_id(post_thread_id)
                        .content(&form.content)
                        .timestamp(rostra_core::Timestamp::now())
                        .ro(state.ro_mode(session.session_token()))
                        .call().await?
                    )
                }
            }

            // Close the preview dialog
            div id="post-preview-dialog" ."o-previewDialog" {}

            // Show success notification
            div id="ajax-scripts" {
                script {
                    (PreEscaped(r#"
                        window.dispatchEvent(new CustomEvent('notify', {
                            detail: { type: 'success', message: 'Reply posted successfully' }
                        }));
                    "#))
                }
            }

            (re_typeset())
        }));
    }

    // Standard new post handling
    // Clear the form content after posting (clear_content = true clears persisted
    // draft)
    let clean_form = state.new_post_form_inner(
        state.ro_mode(session.session_token()),
        Some(client_ref.rostra_id()),
        true,
    );

    let self_id = client_ref.rostra_id();
    let event_id = event.event_id.to_short();

    Ok(Maud(html! {
        // new clean form (this is the main target)
        (clean_form)

        // Close the preview dialog (x-sync will update this)
        div id="post-preview-dialog" ."o-previewDialog" {}

        // Clear the preview
        div id="new-post-preview" ."o-mainBarTimeline__item -preview -empty" {}

        // Add the newly created post after the placeholder (x-merge=after on target)
        // The content inside this div gets inserted after the target element
        div id="new-post-added" {
            div ."o-mainBarTimeline__item -post" {
                (state.render_post_context(
                    &client_ref,
                    self_id
                    )
                    .event_id(event_id)
                    .post_thread_id(event_id)
                    .content(&form.content)
                    .timestamp(rostra_core::Timestamp::now())
                    .ro(state.ro_mode(session.session_token()))
                    .call().await?
                )
            }
        }

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

        (re_typeset())
    }))
}

pub async fn post_post_preview_dialog(
    state: State<SharedState>,
    session: UserSession,
    cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    headers: HeaderMap,
    Form(form): Form<PostInput>,
) -> RequestResult<impl IntoResponse> {
    // Determine redirect target for no-JS: use form field, fall back to Referer
    let redirect_to = form.redirect.clone().or_else(|| {
        headers
            .get(axum::http::header::REFERER)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    });

    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let self_id = client_ref.rostra_id();

    if form.content.trim().is_empty() {
        return Err(crate::error::RequestError::User {
            source: crate::error::BadRequestSnafu {
                message: "Post content cannot be empty",
            }
            .build(),
        });
    }

    let personas = client_ref.db().get_personas_for_id(self_id).await;
    let saved_persona = cookies.get_persona(self_id);

    let preview_content = state
        .render_post_context(&client.client_ref()?, self_id)
        .content(&form.content)
        .timestamp(rostra_core::Timestamp::now())
        .ro(state.ro_mode(session.session_token()))
        .call()
        .await?;

    // AJAX path: return the dialog overlay fragment
    if is_ajax {
        return Ok(Maud(html! {
            div id="post-preview-dialog" ."o-previewDialog -active" {
                (fragment::dialog_escape_handler("post-preview-dialog"))
                div ."o-previewDialog__content" {
                    h4 ."o-previewDialog__title" { "Post Preview" }
                    div ."o-previewDialog__post" {
                        (preview_content)
                    }

                    @let ajax_attrs = fragment::AjaxLoadingAttrs::for_class("o-previewDialog__submitButton");
                    @let x_target = if let (Some(post_thread_id), Some(reply_to)) = (form.post_thread_id, form.reply_to) {
                        let reply_to_id = reply_to.event_id().to_short();
                        let form_id = post_inline_reply_form_html_id(post_thread_id, reply_to_id);
                        let preview_id = post_inline_reply_preview_html_id(post_thread_id, reply_to_id);
                        let added_id = post_inline_reply_added_html_id(post_thread_id, reply_to_id);
                        format!("{form_id} {preview_id} {added_id} post-preview-dialog ajax-scripts")
                    } else {
                        "new-post-form post-preview-dialog new-post-preview new-post-added ajax-scripts".to_string()
                    };
                    div ."o-previewDialog__actions" {
                        form ."o-previewDialog__form"
                            action="/post"
                            method="post"
                            x-target=(x_target)
                            "x-on:keyup.enter.ctrl.shift"="$el.requestSubmit()"
                            "@ajax:before"=(ajax_attrs.before)
                            "@ajax:after"=(ajax_attrs.after)
                        {
                            input type="hidden" name="content" value=(form.content) {}
                            @if let Some(reply_to) = form.reply_to {
                                input type="hidden" name="reply_to" value=(reply_to) {}
                            }
                            @if let Some(post_thread_id) = form.post_thread_id {
                                input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                            }

                            div ."o-previewDialog__actionContainer" {
                                div ."o-previewDialog__personaContainer" {
                                    select
                                        name="persona"
                                        id="persona-select"
                                        ."o-previewDialog__personaSelect"
                                        x-autofocus
                                    {
                                        @for (persona_id, persona_display_name) in &personas {
                                            option
                                                value=(persona_id)
                                                selected[saved_persona.map_or(false, |id| PersonaId(id) == *persona_id)]
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
                (re_typeset())
            }
        }));
    }

    // No-JS path: render full page with preview and submit form
    let body = html! {
        div ."o-mainBarTimeline__item" {
            (preview_content)
        }
        div ."o-mainBarTimeline__item" {
            form action="/post" method="post" {
                input type="hidden" name="content" value=(form.content) {}
                @if let Some(ref redirect) = redirect_to {
                    input type="hidden" name="redirect" value=(redirect) {}
                }
                @if let Some(reply_to) = form.reply_to {
                    input type="hidden" name="reply_to" value=(reply_to) {}
                }
                @if let Some(post_thread_id) = form.post_thread_id {
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                }

                div ."o-previewDialog__actionContainer" {
                    div ."o-previewDialog__personaContainer" {
                        select name="persona" ."o-previewDialog__personaSelect" {
                            @for (persona_id, persona_display_name) in &personas {
                                option
                                    value=(persona_id)
                                    selected[saved_persona.map_or(false, |id| PersonaId(id) == *persona_id)]
                                { (persona_display_name) }
                            }
                        }
                    }

                    (fragment::button("o-previewDialog__submitButton", "Post").call())
                }
            }
        }
    };

    Ok(Maud(
        state
            .render_nojs_full_page(&session, "Post Preview", body)
            .await?,
    ))
}

pub async fn get_new_post_preview(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<PostInput>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let self_id = client.client_ref()?.rostra_id();
    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div id="new-post-preview" ."o-mainBarTimeline__item -preview"
                ."-reply"[form.reply_to.is_some()]
                ."-post"[form.reply_to.is_none()]
            {
                (state.render_post_context(
                    &client.client_ref()?,
                    self_id
                    )
                    .content(&form.content)
                    .timestamp(rostra_core::Timestamp::now())
                    .ro(state.ro_mode(session.session_token()))
                    .call().await?
                )
                (scroll_new_post_preview_into_view())
                (focus_on_new_post_content_input())
                (re_typeset())
            }
        } @else {
            div id="new-post-preview" ."o-mainBarTimeline__item -preview -empty" { }
        }
    }))
}

#[derive(Deserialize)]
pub struct InlineReplyInput {
    reply_to: ExternalEventId,
    post_thread_id: ShortEventId,
}

/// Handler for inline reply form - renders reply form below a post
/// Also loads existing comments (like the Replies button does)
pub async fn get_inline_reply(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    headers: HeaderMap,
    Query(form): Query<InlineReplyInput>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;
    let self_id = client_ref.rostra_id();
    let reply_to_id = form.reply_to.event_id().to_short();

    // Load existing comments
    let (comments, _) = client_ref
        .db()
        .paginate_social_post_comments_rev(reply_to_id, None, 100)
        .await;

    // AJAX path: return the fragment
    if is_ajax {
        let form_markup = state.render_inline_reply_form(
            form.reply_to,
            form.post_thread_id,
            self_id,
            state.ro_mode(session.session_token()),
        );

        let replies_id = super::post::post_replies_html_id(form.post_thread_id, reply_to_id);
        let preview_id = post_inline_reply_preview_html_id(form.post_thread_id, reply_to_id);
        let added_id = post_inline_reply_added_html_id(form.post_thread_id, reply_to_id);

        return Ok(Maud(html! {
            // Replies container wraps everything
            div id=(replies_id) ."m-postView__replies" {
                // The form (already has its own wrapper div with id)
                (form_markup)

                // Empty preview placeholder
                div id=(preview_id) {}

                // Added placeholder for new replies
                div id=(added_id) x-merge="after" {}

                // Existing replies
                @for comment in comments {
                    @if let Some(djot_content) = comment.content.djot_content.as_ref() {
                        div ."o-postOverview__repliesItem" {
                            (state.render_post_context(
                                &client_ref,
                                comment.author
                                ).event_id(comment.event_id)
                                .post_thread_id(form.post_thread_id)
                                .content(djot_content)
                                .reply_count(comment.reply_count)
                                .timestamp(comment.ts)
                                .ro(state.ro_mode(session.session_token()))
                                .call().await?)
                        }
                    }
                }

                (re_typeset())
            }
        }));
    }

    // No-JS path: render full page with reply form
    let redirect_to = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok());

    // Load the parent post for context
    let parent_post = client_ref.db().get_social_post(reply_to_id).await;

    let body = html! {
        // Show the post being replied to
        @if let Some(ref parent) = parent_post {
            @if let Some(djot_content) = parent.content.djot_content.as_ref() {
                div ."o-mainBarTimeline__item" {
                    (state.render_post_context(
                        &client_ref,
                        parent.author
                        ).event_id(parent.event_id)
                        .post_thread_id(form.post_thread_id)
                        .content(djot_content)
                        .reply_count(parent.reply_count)
                        .timestamp(parent.ts)
                        .ro(state.ro_mode(session.session_token()))
                        .call().await?)
                }
            }
        }

        // Reply form that submits to preview_dialog for a preview step
        div ."o-mainBarTimeline__item" {
            form action="/post/preview_dialog" method="post" {
                input type="hidden" name="reply_to" value=(form.reply_to) {}
                input type="hidden" name="post_thread_id" value=(form.post_thread_id) {}
                @if let Some(redirect) = redirect_to {
                    input type="hidden" name="redirect" value=(redirect) {}
                }

                textarea
                    ."m-nojs-textarea"
                    name="content"
                    placeholder="Your reply..."
                    dir="auto"
                    rows="4"
                    {}

                div ."o-previewDialog__actionContainer" {
                    div {}
                    (fragment::button("o-previewDialog__submitButton", "Preview").call())
                }
            }
        }
    };

    Ok(Maud(
        state
            .render_nojs_full_page(&session, "Post Reply", body)
            .await?,
    ))
}

/// Handler for canceling/clearing inline reply form - returns empty
/// placeholders
pub async fn get_inline_reply_cancel(Query(form): Query<InlineReplyInput>) -> impl IntoResponse {
    let form_id =
        post_inline_reply_form_html_id(form.post_thread_id, form.reply_to.event_id().to_short());
    let preview_id =
        post_inline_reply_preview_html_id(form.post_thread_id, form.reply_to.event_id().to_short());

    Maud(html! {
        div id=(form_id) {}
        div id=(preview_id) {}
    })
}

#[derive(Deserialize)]
pub struct InlineReplyPreviewInput {
    reply_to: Option<ExternalEventId>,
    content: String,
    post_thread_id: ShortEventId,
}

/// Handler for inline preview updates
pub async fn post_inline_reply_preview(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<InlineReplyPreviewInput>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;
    let self_id = client_ref.rostra_id();
    let reply_to_event_id = form.reply_to.map(|r| r.event_id().to_short());
    let preview_id = post_inline_reply_preview_html_id(
        form.post_thread_id,
        reply_to_event_id.unwrap_or(form.post_thread_id),
    );

    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div id=(preview_id) ."m-inlineReply__preview -active" {
                (state.render_post_context(
                    &client_ref,
                    self_id
                    )
                    .content(&form.content)
                    .timestamp(rostra_core::Timestamp::now())
                    .ro(state.ro_mode(session.session_token()))
                    .call().await?
                )
                (re_typeset())
            }
        } @else {
            div id=(preview_id) ."m-inlineReply__preview" { }
        }
    }))
}

fn focus_on_inline_reply_content(textarea_id: &str) -> Markup {
    html! {
        script {
            (PreEscaped(format!(r#"
                (function() {{
                    const content = document.getElementById('{textarea_id}');
                    if (content != null) {{
                        content.focus();
                    }}
                }})()
            "#)))
        }
    }
}

impl UiState {
    fn render_inline_reply_form(
        &self,
        reply_to: ExternalEventId,
        post_thread_id: ShortEventId,
        self_id: RostraId,
        ro: RoMode,
    ) -> Markup {
        let reply_to_id = reply_to.event_id().to_short();
        let id_suffix = format!("{post_thread_id}-{reply_to_id}");
        let form_id = post_inline_reply_form_html_id(post_thread_id, reply_to_id);
        let preview_id = post_inline_reply_preview_html_id(post_thread_id, reply_to_id);
        let attach_form_id = format!("inline-reply-attach-form-{id_suffix}");
        let cancel_target = format!("{form_id} {preview_id}");

        html! {
            div id=(form_id) ."m-inlineReply -active" {
                // Hidden form for inline reply preview updates
                @let inline_reply_preview_form_id = format!("inline-reply-preview-form-{id_suffix}");
                form id=(inline_reply_preview_form_id)
                    action="/post/inline_reply_preview"
                    method="post"
                    style="display: none;"
                    x-target=(preview_id)
                {
                    input type="hidden" name="content" value="" {}
                    input type="hidden" name="reply_to" value=(reply_to) {}
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                }

                // Main form (textarea + Preview button)
                @let form_ajax = fragment::AjaxLoadingAttrs::for_class("m-inlineReply__previewButton");
                form ."m-inlineReply__form"
                    action="/post/preview_dialog"
                    method="post"
                    x-target="post-preview-dialog"
                    "@ajax:before"=(form_ajax.before)
                    "@ajax:after"=(form_ajax.after)
                {
                    input type="hidden" name="reply_to" value=(reply_to) {}
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}

                    div ."m-inlineReply__textareaWrapper"
                        x-data="textAutocomplete"
                        style="position: relative;"
                    {
                        @let input_handler = format!(r#"
                            handleInput($event);
                            const previewForm = document.getElementById('{inline_reply_preview_form_id}');
                            const contentInput = previewForm.querySelector('input[name=content]');
                            contentInput.value = $el.value;
                            previewForm.requestSubmit();
                        "#);
                        @let textarea_id = format!("inline-reply-content-{id_suffix}");
                        textarea
                            id=(textarea_id)
                            ."m-inlineReply__content"
                            placeholder="Your reply..."
                            dir="auto"
                            name="content"
                            "@input"=(input_handler)
                            "@keydown"="handleKeydown($event)"
                            autocomplete="off"
                            disabled[ro.to_disabled()]
                            "x-on:keyup.enter.ctrl"="$el.form.requestSubmit()"
                            {}

                        // Autocomplete dropdown (mentions and emojis)
                        div ."m-textAutocomplete"
                            x-show="showDropdown"
                            x-cloak
                            "@click.outside"="showDropdown = false"
                        {
                            template x-if="autocompleteType === 'mention'" {
                                div {
                                    template x-for="(result, index) in results" ":key"="result.rostra_id" {
                                        div ."m-textAutocomplete__item"
                                            ":class"="{ '-selected': index === selectedIndex }"
                                            "@click"="selectResult(result)"
                                        {
                                            span ."m-textAutocomplete__displayName" x-text="result.display_name" {}
                                            span ."m-textAutocomplete__id" x-text="'@' + result.rostra_id.substring(0, 8)" {}
                                        }
                                    }
                                }
                            }
                            template x-if="autocompleteType === 'emoji'" {
                                div {
                                    template x-for="(result, index) in results" ":key"="index" {
                                        div ."m-textAutocomplete__item"
                                            ":class"="{ '-selected': index === selectedIndex }"
                                            "@click"="selectResult(result)"
                                        {
                                            span ."m-textAutocomplete__emoji" x-text="result.emoji" {}
                                            span ."m-textAutocomplete__shortcode" x-text="':' + result.shortcode + ':'" {}
                                        }
                                    }
                                }
                            }
                            div x-show="results.length === 0 && query.length > 0" ."m-textAutocomplete__empty" {
                                "No matches found"
                            }
                        }
                    }

                    div ."m-inlineReply__footer" {
                        @let cancel_form_id = format!("inline-reply-cancel-form-{id_suffix}");
                        @let cancel_onclick = format!(r#"
                            const textarea = document.getElementById('inline-reply-content-{id_suffix}');
                            if (textarea && textarea.value.trim() !== '') {{
                                if (!confirm('Discard your reply?')) {{
                                    event.preventDefault();
                                    return false;
                                }}
                            }}
                        "#);
                        @let emoji_picker_id = format!("emoji-picker-{id_suffix}");
                        @let emoji_onclick = format!("toggleEmojiPicker('{emoji_picker_id}', event)");
                        div ."m-inlineReply__footerLeft" {
                            a ."m-inlineReply__helpButton"
                                href="https://htmlpreview.github.io/?https://github.com/jgm/djot/blob/master/doc/syntax.html"
                                target="_blank"
                                title="Formatting help"
                            {
                                span ."m-inlineReply__helpButtonIcon" {}
                            }
                            a ."m-inlineReply__emojiButton"
                                href="#"
                                title="Insert emoji"
                                onclick=(emoji_onclick)
                            { "😀" }
                            button
                                ."m-inlineReply__attachButton"
                                type="submit"
                                form=(attach_form_id)
                                title="Attach media"
                                disabled[ro.to_disabled()]
                            {
                                span ."m-inlineReply__attachButtonIcon" {}
                            }
                            button
                                ."m-inlineReply__cancelButton"
                                type="submit"
                                form=(cancel_form_id)
                                title="Cancel"
                                onclick=(cancel_onclick)
                            {
                                span ."m-inlineReply__cancelButtonIcon" {}
                            }
                        }
                        (fragment::button("m-inlineReply__previewButton", "Preview")
                            .disabled(ro.to_disabled())
                            .call())
                    }
                }

                // Cancel form (outside main form to avoid nesting)
                @let cancel_ajax = fragment::AjaxLoadingAttrs::for_document_class("m-inlineReply__cancelButton");
                form id=(cancel_form_id)
                    action="/post/inline_reply_cancel"
                    method="get"
                    x-target=(cancel_target)
                    style="display: none;"
                    "@ajax:before"=(cancel_ajax.before)
                    "@ajax:after"=(cancel_ajax.after)
                {
                    input type="hidden" name="reply_to" value=(reply_to) {}
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                }

                // Attach form (outside main form to avoid nesting)
                @let attach_ajax = fragment::AjaxLoadingAttrs::for_document_class("m-inlineReply__attachButton");
                @let textarea_selector = format!("#inline-reply-content-{id_suffix}");
                form id=(attach_form_id)
                    action=(format!("/media/{}/list", self_id))
                    method="get"
                    x-target="media-list"
                    style="display: none;"
                    "@ajax:before"=(attach_ajax.before)
                    "@ajax:after"=(attach_ajax.after)
                {
                    input type="hidden" name="target" value=(textarea_selector) {}
                }

                // Emoji picker for this inline reply
                div id=(emoji_picker_id) ."m-inlineReply__emojiBar -hidden"
                    data-textarea-selector=(format!("#inline-reply-content-{id_suffix}"))
                {
                    emoji-picker
                        data-source="/assets/libs/emoji-picker-element/data.json"
                    {}
                }

                // Note: preview container is added by caller as a sibling, not here

                (focus_on_inline_reply_content(&textarea_id))
            }
        }
    }

    pub fn new_post_form(
        &self,
        _notification: impl Into<Option<Markup>>,
        ro: RoMode,
        user_id: Option<RostraId>,
    ) -> Markup {
        self.new_post_form_inner(ro, user_id, false)
    }

    fn new_post_form_inner(
        &self,
        ro: RoMode,
        user_id: Option<RostraId>,
        clear_content: bool,
    ) -> Markup {
        html! {
            // Hidden form for new post preview updates (must be outside main form)
            form id="new-post-preview-form"
                action="/post/new_post_preview"
                method="post"
                style="display: none;"
                x-target="new-post-preview"
                x-autofocus
            {
                input type="hidden" name="content" value="" {}
            }

            @let form_ajax = fragment::AjaxLoadingAttrs::for_class("m-newPostForm__previewButton");
            form id="new-post-form" ."m-newPostForm"
                action="/post/preview_dialog"
                method="post"
                x-target="post-preview-dialog"
                x-data="{ content: $persist('').as('new-post-content') }"
                x-init=[clear_content.then_some("content = ''")]
                "@ajax:before"=(form_ajax.before)
                "@ajax:after"=(form_ajax.after)
            {
                div ."m-newPostForm__textareaWrapper"
                    x-data="textAutocomplete"
                    style="position: relative;"
                {
                    textarea
                        id="new-post-content"
                        ."m-newPostForm__content"
                        placeholder=(
                            if ro.to_disabled() {
                                "Read-only view. Logout to change."
                            } else {
                              "What's on your mind?"
                            })
                        dir="auto"
                        name="content"
                        x-model="content"
                        "@input"=r#"
                            handleInput($event);
                            // Also handle new post preview update
                            const previewForm = document.getElementById('new-post-preview-form');
                            const contentInput = previewForm.querySelector('input[name=content]');
                            contentInput.value = $el.value;
                            previewForm.requestSubmit();
                        "#
                        "@keydown"="handleKeydown($event)"
                        autocomplete="off"
                        autofocus
                        x-autofocus
                        disabled[ro.to_disabled()]
                        "x-on:keyup.enter.ctrl"="$el.form.requestSubmit()"
                        {}

                    // Autocomplete dropdown (mentions and emojis)
                    div ."m-textAutocomplete"
                        x-show="showDropdown"
                        x-cloak
                        "@click.outside"="showDropdown = false"
                    {
                        // Mention results
                        template x-if="autocompleteType === 'mention'" {
                            div {
                                template x-for="(result, index) in results" ":key"="result.rostra_id" {
                                    div ."m-textAutocomplete__item"
                                        ":class"="{ '-selected': index === selectedIndex }"
                                        "@click"="selectResult(result)"
                                    {
                                        span ."m-textAutocomplete__displayName" x-text="result.display_name" {}
                                        span ."m-textAutocomplete__id" x-text="'@' + result.rostra_id.substring(0, 8)" {}
                                    }
                                }
                            }
                        }
                        // Emoji results
                        template x-if="autocompleteType === 'emoji'" {
                            div {
                                template x-for="(result, index) in results" ":key"="index" {
                                    div ."m-textAutocomplete__item"
                                        ":class"="{ '-selected': index === selectedIndex }"
                                        "@click"="selectResult(result)"
                                    {
                                        span ."m-textAutocomplete__emoji" x-text="result.emoji" {}
                                        span ."m-textAutocomplete__shortcode" x-text="':' + result.shortcode + ':'" {}
                                    }
                                }
                            }
                        }
                        div x-show="results.length === 0 && query.length > 0" ."m-textAutocomplete__empty" {
                            "No matches found"
                        }
                    }
                }

                div ."m-newPostForm__footer" {
                    div ."m-newPostForm__footerRow m-newPostForm__footerRow--main" {
                        a ."m-newPostForm__helpButton"
                            href="https://htmlpreview.github.io/?https://github.com/jgm/djot/blob/master/doc/syntax.html"
                            target="_blank"
                            title="Formatting help"
                        {
                            span ."m-newPostForm__helpButtonIcon" {}
                        }
                        a
                            ."m-newPostForm__emojiButton"
                            href="#"
                            onclick="toggleEmojiPicker('emoji-picker-container', event)"
                        { "😀" }
                        @if user_id.is_some() {
                            button
                                ."m-newPostForm__attachButton"
                                type="submit"
                                form="media-attach-form"
                                title="Attach media"
                                disabled[ro.to_disabled()]
                            {
                                span ."m-newPostForm__attachButtonIcon" {}
                            }
                        } @else {
                            button
                                ."m-newPostForm__attachButton"
                                type="button"
                                title="Attach media"
                                disabled
                            {
                                span ."m-newPostForm__attachButtonIcon" {}
                            }
                        }
                        (fragment::button("m-newPostForm__previewButton", "Preview")
                            .disabled(ro.to_disabled())
                            .call())
                    }
                }
            }

            // Separate forms for media operations (outside main form to avoid nesting)
            @if let Some(uid) = user_id {
                @let attach_ajax = fragment::AjaxLoadingAttrs::for_document_class("m-newPostForm__attachButton");
                form id="media-attach-form"
                    action=(format!("/media/{}/list", uid))
                    method="get"
                    x-target="media-list"
                    style="display: none;"
                    "@ajax:before"=(attach_ajax.before)
                    "@ajax:after"=(attach_ajax.after)
                {
                    input type="hidden" name="target" value="#new-post-content" {}
                }

                input id="media-file-input"
                    name="media_file"
                    type="file"
                    style="display: none;"
                    "@change"="uploadMediaFile($el)"
                    {}
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
