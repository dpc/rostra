use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Form;
use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_client::ClientRef;
use rostra_client_db::IdSocialProfileRecord;
use rostra_client_db::social::{
    EventPaginationCursor, ReceivedAtPaginationCursor, SocialPostRecord,
};
use rostra_core::event::{EventKind, PersonaId, PersonaSelector, SocialPost};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::FmtCompact as _;
use serde::Deserialize;
use tower_cookies::Cookies;
use tracing::{debug, warn};

use super::super::error::RequestResult;
use super::Maud;
use super::cookies::CookiesExt as _;
use super::unlock::session::UserSession;
use crate::html_utils::re_typeset_mathjax;
use crate::layout::FeedLinks;
use crate::util::extractors::AjaxRequest;
use crate::{LOG_TARGET, SharedState, UiState};

#[derive(Deserialize)]
pub struct TimelinePaginationInput {
    pub ts: Option<Timestamp>,
    pub seq: Option<u64>,
    pub event_id: Option<ShortEventId>,
}

pub async fn get_followees(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| TimelineCursor::ByEventTime(EventPaginationCursor { ts, event_id }))
    });
    let navbar = state.timeline_common_navbar(&session).await?;
    Ok(Maud(
        state
            .render_timeline_page(
                navbar,
                pagination,
                &session,
                &mut cookies,
                TimelineMode::Followees,
                is_ajax,
            )
            .await?,
    ))
}

pub async fn get_network(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| TimelineCursor::ByEventTime(EventPaginationCursor { ts, event_id }))
    });
    let navbar = state.timeline_common_navbar(&session).await?;
    Ok(Maud(
        state
            .render_timeline_page(
                navbar,
                pagination,
                &session,
                &mut cookies,
                TimelineMode::Network,
                is_ajax,
            )
            .await?,
    ))
}

pub async fn get_notifications(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.seq
            .map(|seq| TimelineCursor::ByReceivedTime(ReceivedAtPaginationCursor { ts, seq }))
    });
    let navbar = state.timeline_common_navbar(&session).await?;
    Ok(Maud(
        state
            .render_timeline_page(
                navbar,
                pagination,
                &session,
                &mut cookies,
                TimelineMode::Notifications,
                is_ajax,
            )
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct UpdatesQuery {
    pub notifications: Option<usize>,
}

pub async fn get_updates(
    state: State<SharedState>,
    ws: WebSocketUpgrade,
    session: UserSession,
    Form(query): Form<UpdatesQuery>,
) -> impl IntoResponse {
    let pending_notifications = query.notifications.unwrap_or(0);
    ws.on_upgrade(move |ws| async move {
        let _ = state
            .handle_get_updates(ws, &session, pending_notifications)
            .await
            .inspect_err(|err| {
                debug!(target: LOG_TARGET, err=%err.fmt_compact(), "WS handler failed");
            });
    })
}

pub async fn get_post_replies(
    state: State<SharedState>,
    session: UserSession,
    Path((post_thread_id, event_id)): Path<(ShortEventId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    Ok(Maud(
        state
            .render_post_replies(post_thread_id, event_id, &session)
            .await?,
    ))
}

#[bon::bon]
impl UiState {
    pub(crate) async fn timeline_common_navbar(
        &self,
        session: &UserSession,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;
        let user_id = client_ref.rostra_id();

        let ro_mode = self.ro_mode(session.session_token());
        Ok(html! {
            nav ."o-navBar" {
                (self.render_top_nav())

                div ."o-navBar__profileSummary" {
                    (self.render_self_profile_summary(session, ro_mode).await?)
                }

                (self.new_post_form(None, ro_mode, Some(user_id)))
            }
        })
    }

    async fn handle_get_updates(
        &self,
        mut ws: WebSocket,
        user: &UserSession,
        initial_pending_notifications: usize,
    ) -> RequestResult<()> {
        let client = self.client(user.id()).await?;
        let client_ref = client.client_ref()?;
        let self_id = client_ref.rostra_id();
        let mut new_posts = client_ref.new_posts_subscribe();

        let followees: HashMap<RostraId, PersonaSelector> = client_ref
            .db()
            .get_followees(self_id)
            .await
            .into_iter()
            .collect();

        let mut followees_count: u64 = 0;
        let mut network_count: u64 = 0;
        let mut notifications_count: u64 = initial_pending_notifications as u64;

        loop {
            let (event_content, social_post) = match new_posts.recv().await {
                Ok(event_content) => event_content,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    continue;
                }
            };
            if event_content.event.event.kind != EventKind::SOCIAL_POST {
                continue;
            }
            let author = event_content.event.event.author;
            if author == self_id {
                continue;
            }

            network_count += 1;

            if followees
                .get(&author)
                .is_some_and(|selector| selector.matches(social_post.persona))
            {
                followees_count += 1;
            }

            let is_reply_to_self =
                social_post.reply_to.map(|ext_id| ext_id.rostra_id()) == Some(self_id);
            let is_self_mention = client_ref
                .db()
                .is_self_mention(event_content.event.event_id.to_short())
                .await;
            if is_reply_to_self || is_self_mention {
                notifications_count += 1;
            }

            let badge_html = self
                .render_tab_badges(followees_count, network_count, notifications_count)
                .into_string();
            let _ = ws.send(badge_html.into()).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    fn render_tab_badges(&self, followees: u64, network: u64, notifications: u64) -> Markup {
        html! {
            span id="followees-new-count"
                ."o-mainBarTimeline__newCount"
                x-swap-oob="outerHTML:#followees-new-count"
            {
                @if 9 < followees { " (9+)" }
                @else if 0 < followees { " (" (followees) ")" }
            }
            span id="network-new-count"
                ."o-mainBarTimeline__newCount"
                x-swap-oob="outerHTML:#network-new-count"
            {
                @if 9 < network { " (9+)" }
                @else if 0 < network { " (" (network) ")" }
            }
            span id="notifications-new-count"
                ."o-mainBarTimeline__pendingNotifications"
                x-swap-oob="outerHTML:#notifications-new-count"
            {
                @if 9 < notifications { " (9+)" }
                @else if 0 < notifications { " (" (notifications) ")" }
            }
        }
    }

    pub(crate) async fn render_timeline_page(
        &self,
        navbar: Markup,
        pagination: Option<TimelineCursor>,
        session: &UserSession,
        cookies: &mut Cookies,
        mode: TimelineMode,
        is_ajax_request: bool,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;
        let (pending_notifications, debug_info) = self
            .handle_notification_cookies(&client_ref, pagination.is_some(), cookies, mode)
            .await?;

        let timeline = self
            .render_main_bar_timeline(session, mode)
            .maybe_pagination(pagination)
            .maybe_pending_notifications(pending_notifications)
            .debug_info(debug_info)
            .call()
            .await?;

        if is_ajax_request {
            return Ok(timeline);
        }
        // Otherwise return the full page
        let page_layout = self.render_page_layout(navbar, timeline);

        let content = html! {
            (page_layout)

            // Dialog containers for timeline interactions
            div id="post-preview-dialog" ."o-previewDialog" x-sync {}
            div id="media-list" ."o-mediaList" x-sync {}
            div id="ajax-scripts" style="display: none;" {}
            div id="follow-dialog-content" {}

            // Initialize emoji picker and make Database available globally
            script type="module" {
                (PreEscaped(r#"
                    import { Picker, Database } from '/assets/libs/emoji-picker-element/index.js';
                    import textFieldEdit from '/assets/libs/text-field-edit/index.js';

                    // Make Database available globally for the Alpine component
                    window.EmojiDatabase = Database;

                    // Handle emoji picker clicks - find the correct textarea from the picker's container
                    document.addEventListener('emoji-click', e => {
                        const picker = e.target;
                        const container = picker.closest('[data-textarea-selector]');
                        let textarea;
                        if (container) {
                            // Inline reply picker - use the data attribute to find the textarea
                            textarea = document.querySelector(container.dataset.textareaSelector);
                        } else {
                            // Main form picker - use default selector
                            textarea = document.getElementById('new-post-content');
                        }
                        if (textarea) {
                            textFieldEdit.insert(textarea, e.detail.unicode);
                        }
                    });
                "#))
            }

            // Initialize text autocomplete (mentions and emojis) once (outside form to avoid re-execution)
            script {
                (PreEscaped(include_str!("text_autocomplete.js")))
            }

            // Initialize insertMediaSyntax function once
            script {
                (PreEscaped(r#"
                    window.insertMediaSyntax = function(eventId, targetSelector) {
                        const textarea = document.querySelector(targetSelector);
                        const syntax = '![media](rostra-media:' + eventId + ')';

                        if (textarea) {
                            const start = textarea.selectionStart;
                            const end = textarea.selectionEnd;
                            const text = textarea.value;

                            const newText = text.substring(0, start) + syntax + text.substring(end);
                            textarea.value = newText;

                            const newPos = start + syntax.length;
                            textarea.setSelectionRange(newPos, newPos);
                            textarea.focus();

                            textarea.dispatchEvent(new Event('input', { bubbles: true }));
                        }

                        const mediaList = document.querySelector('.o-mediaList');
                        if (mediaList) {
                            mediaList.style.display = 'none';
                        }
                    };
                "#))
            }

            (re_typeset_mathjax())

        };

        // Build feed links for profile pages
        let feed_links = match mode {
            TimelineMode::Profile(profile_id) => {
                let profile = self.get_social_profile(profile_id, &client_ref).await;
                Some(FeedLinks {
                    title: format!("{} - Rostra Feed", profile.display_name),
                    atom_url: format!("/profile/{profile_id}/atom.xml"),
                })
            }
            _ => None,
        };

        self.render_html_page("Rostra", content, feed_links.as_ref())
            .await
    }

    pub(crate) async fn handle_notification_cookies(
        &self,
        client: &ClientRef<'_>,
        is_paginated: bool,
        cookies: &mut Cookies,
        mode: TimelineMode,
    ) -> RequestResult<(Option<usize>, super::debug::NotificationDebugInfo)> {
        use super::debug::NotificationDebugInfo;

        // If this is a non-first page, we don't need to do anything
        if is_paginated {
            return Ok((None, NotificationDebugInfo::default()));
        }

        match mode {
            TimelineMode::Profile(_) | TimelineMode::ProfileSingle(_, _) => {
                // We're not displaying notifications on profile timelines
                Ok((None, NotificationDebugInfo::default()))
            }
            TimelineMode::Notifications => {
                // Save the cursor of the most recently received post as the last seen marker
                let latest_cursor = client
                    .db()
                    .get_latest_social_post_received_at_cursor()
                    .await;
                if let Some(cursor) = latest_cursor {
                    cookies.save_last_seen(client.rostra_id(), cursor);
                }
                Ok((None, NotificationDebugInfo::for_save(mode, latest_cursor)))
            }
            TimelineMode::Followees | TimelineMode::Network => {
                // Count pending notifications using received_at ordering
                // Start AFTER the last seen cursor (exclusive) using cursor.next()
                let cookie_cursor = cookies.get_last_seen(client.rostra_id());
                let start_cursor = cookie_cursor.map(|c| c.next());
                let latest_cursor = client
                    .db()
                    .get_latest_social_post_received_at_cursor()
                    .await;
                let (posts, _) = client
                    .db()
                    .paginate_social_posts_by_received_at(
                        start_cursor,
                        10,
                        TimelineMode::Notifications.to_filter_fn(client).await,
                    )
                    .await;
                let pending_len = posts.len();

                Ok((
                    Some(pending_len),
                    NotificationDebugInfo::for_count(
                        mode,
                        cookie_cursor,
                        start_cursor,
                        latest_cursor,
                        pending_len,
                    ),
                ))
            }
        }
    }

    pub async fn render_post_replies(
        &self,
        post_thread_id: ShortEventId,
        post_id: ShortEventId,
        session: &UserSession,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        // Note: we actually are not doing any pagination
        let (comments, _) = self
            .client(session.id())
            .await?
            .db()?
            .paginate_social_post_comments_rev(post_id, None, 100)
            .await;

        Ok(html! {
            // Replies container with placeholders inside
            div ."m-postView__replies"
                id=(super::post::post_replies_html_id(post_thread_id, post_id))
            {
                // Placeholders for inline reply (form, preview, added)
                div id=(super::post::post_inline_reply_form_html_id(post_thread_id, post_id)) {}
                div id=(super::post::post_inline_reply_preview_html_id(post_thread_id, post_id)) {}
                div id=(super::post::post_inline_reply_added_html_id(post_thread_id, post_id)) x-merge="after" {}

                // Existing replies
                @for comment in comments {
                    @if let Some(djot_content) = comment.content.djot_content.as_ref() {
                        div ."o-postOverview__repliesItem" {
                            (self.render_post_context(
                                &client_ref,
                                comment.author
                                ).event_id(comment.event_id)
                                .post_thread_id(post_thread_id)
                                .content(djot_content)
                                .reply_count(comment.reply_count)
                                .timestamp(comment.ts)
                                .ro(self.ro_mode(session.session_token()))
                                .call().await?)
                        }
                    }
                }

                (re_typeset_mathjax())
            }
        })
    }

    #[builder]
    pub(crate) async fn render_main_bar_timeline(
        &self,
        #[builder(start_fn)] session: &UserSession,
        #[builder(start_fn)] mode: TimelineMode,
        pagination: Option<TimelineCursor>,
        pending_notifications: Option<usize>,
        debug_info: Option<super::debug::NotificationDebugInfo>,
    ) -> RequestResult<Markup> {
        let pending_notifications = pending_notifications.unwrap_or_default();
        let debug_info = debug_info.unwrap_or_default();
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        let (filtered_posts, cursor) = mode.get_posts(&client_ref, pagination).await;

        let parents = self
            .client(session.id())
            .await?
            .db()?
            .get_posts_by_id(
                filtered_posts
                    .iter()
                    .flat_map(|post| post.reply_to.map(|ext_id| ext_id.event_id().to_short())),
            )
            .await;

        let author_personas: HashSet<(RostraId, PersonaId)> = filtered_posts
            .iter()
            .map(|post| (post.author, post.content.persona))
            .chain(
                parents
                    .iter()
                    .map(|post| (post.1.author, post.1.content.persona)),
            )
            .collect();

        let author_personas = client.db()?.get_personas(author_personas.into_iter()).await;

        Ok(html! {
            div ."o-mainBarTimeline" x-data=(format!("websocket('/updates?notifications={}')", pending_notifications)) {
                // Workaround: first alpine-ajax request scrolls to top; prime it on load
                div style="display:none" x-init="$ajax('/timeline/prime', { targets: ['timeline-posts'] })" {}
                div ."o-mainBarTimeline__tabs" {
                    a ."o-mainBarTimeline__back" onclick="history.back()" { "<" }

                    @if let TimelineMode::Profile(_) = mode {
                        a ."o-mainBarTimeline__profile"
                            ."-active"[mode.is_profile()]
                            href=(mode.to_path())
                        { "Profile" }

                    } @else {

                        a ."o-mainBarTimeline__followees"
                            ."-active"[mode.is_followees()]
                            href=(TimelineMode::Followees.to_path())
                        {
                            "Followees"
                            span id="followees-new-count" ."o-mainBarTimeline__newCount" {}
                        }
                        a ."o-mainBarTimeline__network"
                            ."-active"[mode.is_network()]
                            href=(TimelineMode::Network.to_path())
                        {
                            "Network"
                            span id="network-new-count" ."o-mainBarTimeline__newCount" {}
                        }
                        a ."o-mainBarTimeline__notifications"
                            ."-active"[mode.is_notifications()]
                            href=(TimelineMode::Notifications.to_path())
                            ."-pending"[0 < pending_notifications]
                        {
                            "Notifications"
                            span id="notifications-new-count" ."o-mainBarTimeline__pendingNotifications" {
                                @if 9 < pending_notifications {
                                    "(9+)"
                                } @else if 0 < pending_notifications {
                                    "("(pending_notifications)")"
                                }
                            }
                        }
                    }
                }
                // DEBUG: notification counting info (enable with ROSTRA_DEBUG_NOTIFICATIONS=1)
                (debug_info.render())
                div ."o-mainBarTimeline__switches" {

                    label ."o-mainBarTimeline__repliesLabel" for="show-replies" { "Replies" }
                    label ."o-mainBarTimeline__repliesToggle switch" {
                        input id="show-replies"
                        ."o-mainBarTimeline__showReplies"
                        type="checkbox" checked
                            onclick="this.closest('.o-mainBarTimeline').classList.toggle('-hideReplies', !this.checked)"
                        { }
                        span class="slider round" { }
                    }
                }
                div id="new-post-preview" ."o-mainBarTimeline__item -preview -empty" x-sync { }
                div id="new-post-added" x-merge="after" {}
                div id="timeline-posts" x-merge="append" {
                    @for post in &filtered_posts {
                        @if let Some(djot_content) = post.content.djot_content.as_ref() {
                            div ."o-mainBarTimeline__item"
                            ."-reply"[post.reply_to.is_some()]
                            ."-post"[post.reply_to.is_none()]
                            {
                                (
                                    self.render_post_context(
                                        &client_ref,
                                        post.author,
                                    ).maybe_persona_display_name(
                                        author_personas.get(&(post.author, post.content.persona)).map(AsRef::as_ref)
                                    )
                                    .maybe_reply_to(
                                        post.reply_to
                                            .map(|reply_to| (
                                                reply_to.rostra_id(),
                                                reply_to.event_id(),
                                                parents.get(&reply_to.event_id().to_short()))
                                            )
                                        )
                                        .event_id(post.event_id)
                                        .post_thread_id(post.event_id)
                                        .content(djot_content)
                                        .reply_count(post.reply_count)
                                        .timestamp(post.ts)
                                        .ro(self.ro_mode(session.session_token()))
                                        .call()
                                        .await?
                                )
                            }
                        }
                    }
                }
                @if let Some(cursor) = cursor {
                    // Infinite scroll: load more posts when approaching bottom
                    // See: https://stackoverflow.com/q/58622664/134409
                    // for why plain `x-intersect` can't be used.
                    @let href = format!("{}?{}", mode.to_path(), cursor.to_query_params());
                    a
                        id="load-more-posts" ."o-mainBarTimeline__rest -empty"
                        "href"=(href)
                        x-init="new IntersectionObserver((entries, obs) => { if (entries[0].isIntersecting) { obs.disconnect(); $ajax($el.href, { targets: ['load-more-posts', 'timeline-posts'] }); } }, { root: document.body, rootMargin: '0px 0px 250% 0px' }).observe($el)"
                    { "More posts" }
                } @else {
                    div id="load-more-posts" ."o-mainBarTimeline__rest -empty" {}
                }
            }
        })
    }

    pub(crate) fn render_user_handle(
        &self,
        _event_id: Option<ShortEventId>,
        id: RostraId,
        profile: Option<&IdSocialProfileRecord>,
    ) -> Markup {
        // TODO: I wanted this to be some kind of a popover etc. but looks
        // like `anchored` css is not there yet
        let display_name = if let Some(profile) = profile {
            profile.display_name.clone()
        } else {
            id.to_short().to_string()
        };
        html! {
            div
                ."a-userNameHandle"
            {
                a
                    ."a-userNameHandle__displayName u-displayName"
                    href={"/profile/"(id)}
                {
                    (display_name)
                }
            }
        }
    }
}

/// Unified cursor for timeline pagination that can hold either event-time or
/// received-time cursors.
///
/// - `ByEventTime`: For Followees/Network/Profile tabs - ordered by author's
///   timestamp
/// - `ByReceivedTime`: For Notifications tab - ordered by when we received the
///   post
#[derive(Copy, Clone, Debug)]
pub(crate) enum TimelineCursor {
    ByEventTime(EventPaginationCursor),
    ByReceivedTime(ReceivedAtPaginationCursor),
}

impl TimelineCursor {
    /// Build query parameters for the "load more" URL
    fn to_query_params(self) -> String {
        match self {
            TimelineCursor::ByEventTime(c) => {
                format!("ts={}&event_id={}", c.ts, c.event_id)
            }
            TimelineCursor::ByReceivedTime(c) => {
                format!("ts={}&seq={}", c.ts, c.seq)
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TimelineMode {
    Followees,
    Network,
    Notifications,
    Profile(RostraId),
    ProfileSingle(RostraId, ShortEventId),
}

impl TimelineMode {
    fn to_path(self) -> String {
        match self {
            TimelineMode::Followees => "/followees".to_string(),
            TimelineMode::Network => "/network".to_string(),
            TimelineMode::Notifications => "/notifications".to_string(),
            TimelineMode::Profile(rostra_id) => format!("/profile/{rostra_id}"),
            TimelineMode::ProfileSingle(rostra_id, _) => format!("/profile/{rostra_id}"),
        }
    }

    fn is_followees(&self) -> bool {
        *self == TimelineMode::Followees
    }
    fn is_network(&self) -> bool {
        *self == TimelineMode::Network
    }
    fn is_notifications(&self) -> bool {
        *self == TimelineMode::Notifications
    }

    fn is_profile(&self) -> bool {
        matches!(self, TimelineMode::Profile(_))
    }
    async fn get_posts(
        self,
        client: &ClientRef<'_>,
        pagination: Option<TimelineCursor>,
    ) -> (Vec<SocialPostRecord<SocialPost>>, Option<TimelineCursor>) {
        if let Self::ProfileSingle(_author, event_id) = self {
            (
                client
                    .db()
                    .get_social_post(event_id)
                    .await
                    .into_iter()
                    .collect(),
                None,
            )
        } else {
            let filter_fn = self.to_filter_fn(client).await;

            // For Notifications, order by when we received posts rather than when
            // they were authored. This ensures new notifications appear at the top.
            if matches!(self, Self::Notifications) {
                let cursor = pagination.and_then(|c| match c {
                    TimelineCursor::ByReceivedTime(c) => Some(c),
                    _ => None,
                });
                let (posts, next) = client
                    .db()
                    .paginate_social_posts_by_received_at_rev(cursor, 20, filter_fn)
                    .await;
                (posts, next.map(TimelineCursor::ByReceivedTime))
            } else {
                let cursor = pagination.and_then(|c| match c {
                    TimelineCursor::ByEventTime(c) => Some(c),
                    _ => None,
                });
                let (posts, next) = client
                    .db()
                    .paginate_social_posts_rev(cursor, 20, filter_fn)
                    .await;
                (posts, next.map(TimelineCursor::ByEventTime))
            }
        }
    }

    #[allow(clippy::type_complexity)]
    async fn to_filter_fn(
        self,
        client: &ClientRef<'_>,
    ) -> Box<dyn Fn(&SocialPostRecord<SocialPost>) -> bool + Send + Sync + 'static> {
        let self_id = client.rostra_id();
        match self {
            TimelineMode::Followees => {
                let followees: HashMap<RostraId, PersonaSelector> = client
                    .db()
                    .get_followees(self_id)
                    .await
                    .into_iter()
                    .collect();
                Box::new(move |post: &SocialPostRecord<SocialPost>| {
                    post.author != self_id
                        && followees
                            .get(&post.author)
                            .is_some_and(|selector| selector.matches(post.content.persona))
                })
            }
            TimelineMode::Network => Box::new(
                // TODO: actually verify against extended followees
                move |post| post.author != self_id,
            ),
            TimelineMode::Notifications => {
                let self_mentions = client.db().get_self_mentions().await;
                Box::new(move |post| {
                    post.author != self_id
                        && (post.reply_to.map(|ext_id| ext_id.rostra_id()) == Some(self_id)
                            || self_mentions.contains(&post.event_id))
                })
            }
            TimelineMode::Profile(rostra_id) => Box::new(move |post| post.author == rostra_id),
            TimelineMode::ProfileSingle(_, _) => {
                warn!(target: LOG_TARGET, "Should not be here");
                Box::new(move |_post| false)
            }
        }
    }
}
