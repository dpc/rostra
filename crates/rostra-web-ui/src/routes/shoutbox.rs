use axum::Form;
use axum::extract::State;
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_client::ClientRef;
use rostra_client_db::social::{ReceivedAtPaginationCursor, ShoutboxPostRecord};
use rostra_core::Timestamp;
use serde::Deserialize;
use tower_cookies::Cookies;

use super::super::SharedState;
use super::super::error::{ReadOnlyModeSnafu, RequestResult};
use super::cookies::CookiesExt as _;
use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::UiState;
use crate::html_utils::re_typeset_mathjax;
use crate::layout::FeedLinks;
use crate::util::extractors::AjaxRequest;
use crate::util::time::format_timestamp;

const SHOUTBOX_LIMIT: usize = 100;

#[derive(Deserialize, Default)]
pub struct ShoutboxPaginationInput {
    pub ts: Option<Timestamp>,
    pub seq: Option<u64>,
    /// If true, this is a request for older messages (prepend to top)
    pub older: Option<bool>,
}

#[derive(Deserialize)]
pub struct ShoutboxPostInput {
    pub content: String,
}

pub async fn get_shoutbox(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Form(form): Form<ShoutboxPaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form
        .ts
        .and_then(|ts| form.seq.map(|seq| ReceivedAtPaginationCursor { ts, seq }));
    let is_loading_older = form.older.unwrap_or(false);

    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let rostra_id = client_ref.rostra_id();
    let ro_mode = state.ro_mode(session.session_token());

    // Get shoutbox posts (newest first from DB)
    let (mut posts, next_cursor) = client_ref
        .db()
        .paginate_shoutbox_posts_by_received_at_rev(pagination, SHOUTBOX_LIMIT)
        .await;

    // Reverse to get oldest first (chronological order for chat display)
    posts.reverse();

    // If this is the first page (no pagination), save the latest cursor as "last
    // seen"
    if pagination.is_none() {
        if let Some(cursor) = client_ref
            .db()
            .get_latest_shoutbox_received_at_cursor()
            .await
        {
            cookies.save_shoutbox_last_seen(rostra_id, cursor);
        }
    }

    // For AJAX requests loading older messages, return just the posts and load-more
    // link
    if is_ajax && is_loading_older {
        return Ok(Maud(html! {
            // Load more link (goes at top, will be replaced)
            @if let Some(cursor) = next_cursor {
                @let href = format!("/shoutbox?ts={}&seq={}&older=true", cursor.ts, cursor.seq);
                a
                    id="shoutbox-load-older"
                    ."o-shoutbox__loadOlder"
                    href=(href)
                    x-target="shoutbox-load-older shoutbox-posts ajax-scripts"
                { "Load older messages" }
            } @else {
                // No more older messages - just hide the load-older element
                div id="shoutbox-load-older" ."o-shoutbox__loadOlder -empty" {}
            }
            // Older posts (prepended to existing)
            div id="shoutbox-posts" x-merge="prepend" {
                @for post in &posts {
                    (state.render_shoutbox_post(&client_ref, post).await?)
                }
            }
            (re_typeset_mathjax())
        }));
    }

    // For regular AJAX updates (new posts arriving), just return new content
    if is_ajax {
        return Ok(Maud(html! {
            div id="shoutbox-posts" x-merge="append" {
                @for post in &posts {
                    (state.render_shoutbox_post(&client_ref, post).await?)
                }
            }
            (re_typeset_mathjax())
        }));
    }

    // WebSocket URL for live updates (start with 0 counts, shoutbox is current page
    // so 0)
    let ws_url =
        "websocket('/updates?followees=0&network=0&notifications=0&shoutbox=0&on_shoutbox=true')";
    let badge_counts = "badgeCounts({ followees: 0, network: 0, notifications: 0, shoutbox: 0 })";

    // Render the shoutbox content with chat-like layout
    let shoutbox_content = html! {
        div ."o-shoutbox"
            x-data=(ws_url)
        {
            // Tab bar (same as other timelines)
            div ."o-mainBarTimeline__tabs"
                x-data=(badge_counts)
                "@badges:updated.window"="onUpdate($event.detail)"
            {
                a ."o-mainBarTimeline__back" href="/" onclick="history.back(); return false;" { "<" }
                a ."o-mainBarTimeline__followees"
                    href="/followees"
                {
                    "Following"
                    span ."o-mainBarTimeline__newCount" x-text="formatCount(followees)" {}
                }
                a ."o-mainBarTimeline__network"
                    href="/network"
                {
                    "Network"
                    span ."o-mainBarTimeline__newCount" x-text="formatCount(network)" {}
                }
                a ."o-mainBarTimeline__notifications"
                    href="/notifications"
                    ":class"="{ '-pending': notifications > 0 }"
                {
                    "Notifications"
                    span ."o-mainBarTimeline__pendingNotifications" x-text="formatCount(notifications)" {}
                }
                a ."o-mainBarTimeline__shoutbox -active"
                    href="/shoutbox"
                    ":class"="{ '-pending': shoutbox > 0 }"
                {
                    "Shoutbox"
                    span ."o-mainBarTimeline__newCount" x-text="formatCount(shoutbox)" {}
                }
            }

            // Scrollable messages area (oldest at top, newest at bottom)
            div ."o-shoutbox__messages" id="shoutbox-messages" {
                // Load older link at top (only if there are more messages to load)
                @if let Some(cursor) = next_cursor {
                    @let href = format!("/shoutbox?ts={}&seq={}&older=true", cursor.ts, cursor.seq);
                    a
                        id="shoutbox-load-older"
                        ."o-shoutbox__loadOlder"
                        href=(href)
                        x-target="shoutbox-load-older shoutbox-posts ajax-scripts"
                    { "Load older messages" }
                } @else if posts.is_empty() {
                    // Only show "no messages" when shoutbox is completely empty
                    div id="shoutbox-load-older" ."o-shoutbox__loadOlder -empty" {
                        span { "No messages yet. Be the first to shout!" }
                    }
                } @else {
                    // Hidden placeholder for when all messages are loaded
                    div id="shoutbox-load-older" ."o-shoutbox__loadOlder -empty" {}
                }

                // Posts list (oldest first)
                div id="shoutbox-posts" ."o-shoutbox__posts" x-merge="append" {
                    @for post in &posts {
                        (state.render_shoutbox_post(&client_ref, post).await?)
                    }
                }
            }

            // Input form fixed at bottom
            @let form_ajax = fragment::AjaxLoadingAttrs::for_class("o-shoutbox__submitButton");
            form ."o-shoutbox__form"
                action="/shoutbox/post"
                method="post"
                "x-target.nofocus"="shoutbox-posts ajax-scripts"
                "@ajax:before"=(form_ajax.before)
                "@ajax:after"=(form_ajax.after)
                "@ajax:success"="setTimeout(() => { const el = document.getElementById('shoutbox-messages'); el.scrollTop = el.scrollHeight; }, 50)"
            {
                div ."o-shoutbox__inputWrapper" {
                    textarea
                        ."o-shoutbox__input"
                        id="shoutbox-input"
                        name="content"
                        placeholder="Shout something..."
                        maxlength="1000"
                        dir="auto"
                        disabled[ro_mode.to_disabled()]
                        rows="1"
                        "@keydown.enter.prevent"="if (!$event.shiftKey) { $el.form.requestSubmit(); }"
                        {}
                    (fragment::button("o-shoutbox__submitButton", "Send")
                        .disabled(ro_mode.to_disabled())
                        .call())
                }
            }
        }
        (re_typeset_mathjax())
        // Auto-scroll to bottom on initial load
        script {
            (PreEscaped(r#"
                (function() {
                    const el = document.getElementById('shoutbox-messages');
                    if (el) el.scrollTop = el.scrollHeight;
                })();
            "#))
        }
    };

    // Full page layout (hide new post form since shoutbox has its own input)
    let navbar = state
        .timeline_common_navbar()
        .session(&session)
        .hide_new_post_form(true)
        .call()
        .await?;
    let page_layout = state.render_page_layout(navbar, shoutbox_content);

    let content = html! {
        (page_layout)
        div id="ajax-scripts" style="display: none;" {}
    };

    Ok(Maud(
        state
            .render_html_page("Shoutbox - Rostra", content, None::<&FeedLinks>)
            .await?,
    ))
}

pub async fn post_shoutbox(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<ShoutboxPostInput>,
) -> RequestResult<impl IntoResponse> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;

    // Validate content
    let content = form.content.trim();
    if content.is_empty() || 1000 < content.len() {
        return Ok(Maud(html! {
            div id="shoutbox-posts" x-merge="append" {}
            div id="ajax-scripts" {
                script {
                    (PreEscaped(r#"
                        window.dispatchEvent(new CustomEvent('notify', {
                            detail: { type: 'error', message: 'Shoutout must be between 1 and 1000 characters' }
                        }));
                    "#))
                }
            }
        }));
    }

    // Post the shoutbox message
    // The post will arrive via WebSocket like any other shout
    let _event = client_ref
        .post_shoutbox(id_secret, content.to_string())
        .await?;

    // Just clear the input - the post will appear via WebSocket
    Ok(Maud(html! {
        div id="shoutbox-posts" x-merge="append" {}
        div id="ajax-scripts" {
            script {
                (PreEscaped(r#"
                    (function() {
                        const input = document.getElementById('shoutbox-input');
                        if (input) input.value = '';
                    })()
                "#))
            }
        }
    }))
}

impl UiState {
    pub(crate) async fn render_shoutbox_post(
        &self,
        client: &ClientRef<'_>,
        post: &ShoutboxPostRecord,
    ) -> RequestResult<Markup> {
        let profile = self.get_social_profile(post.author, client).await;

        Ok(html! {
            div ."o-shoutbox__post" {
                (fragment::avatar("o-shoutbox__avatar", self.avatar_url(post.author), "Avatar"))
                div ."o-shoutbox__postBody" {
                    div ."o-shoutbox__postMeta" {
                        a ."o-shoutbox__author" href=(format!("/profile/{}", post.author)) {
                            (profile.display_name)
                        }
                        span ."o-shoutbox__timestamp" {
                            (format_timestamp(post.received_ts))
                        }
                    }
                    div ."o-shoutbox__postContent" {
                        (self.render_content(client, post.author, &post.content.djot_content).await)
                    }
                }
            }
        })
    }
}
