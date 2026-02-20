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
use rostra_core::event::{EventKind, PersonaId, PersonaSelector, SocialPost, content_kind};
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

#[derive(Default, Clone, Copy)]
pub struct PendingCounts {
    pub followees: usize,
    pub network: usize,
    pub notifications: usize,
    pub shoutbox: usize,
}

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
    let navbar = state
        .timeline_common_navbar()
        .session(&session)
        .call()
        .await?;
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
    let navbar = state
        .timeline_common_navbar()
        .session(&session)
        .call()
        .await?;
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
    let navbar = state
        .timeline_common_navbar()
        .session(&session)
        .call()
        .await?;
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

#[derive(Deserialize, Default)]
pub struct UpdatesQuery {
    pub followees: Option<usize>,
    pub network: Option<usize>,
    pub notifications: Option<usize>,
    pub shoutbox: Option<usize>,
    /// If true, we're on the shoutbox page - skip shoutbox counter updates
    pub on_shoutbox: Option<bool>,
}

pub async fn get_updates(
    state: State<SharedState>,
    ws: WebSocketUpgrade,
    session: UserSession,
    Form(query): Form<UpdatesQuery>,
) -> impl IntoResponse {
    let pending = PendingCounts {
        followees: query.followees.unwrap_or(0),
        network: query.network.unwrap_or(0),
        notifications: query.notifications.unwrap_or(0),
        shoutbox: query.shoutbox.unwrap_or(0),
    };
    let on_shoutbox = query.on_shoutbox.unwrap_or(false);
    ws.on_upgrade(move |ws| async move {
        let _ = state
            .handle_get_updates(ws, &session, pending, on_shoutbox)
            .await
            .inspect_err(|err| {
                debug!(target: LOG_TARGET, err=%err.fmt_compact(), "WS handler failed");
            });
    })
}

pub async fn get_post_replies(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Path((post_thread_id, event_id)): Path<(ShortEventId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    // AJAX path: return the fragment
    if is_ajax {
        return Ok(Maud(
            state
                .render_post_replies(post_thread_id, event_id, &session)
                .await?,
        ));
    }

    // No-JS path: render full page with parent post and replies
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    let parent_post = client_ref.db().get_social_post(event_id).await;

    let (comments, _) = client_ref
        .db()
        .paginate_social_post_comments_rev(event_id, None, 100)
        .await;

    let body = html! {
        // Show the parent post
        @if let Some(ref parent) = parent_post {
            div ."o-mainBarTimeline__item" {
                (state.render_post_context(
                    &client_ref,
                    parent.author
                    ).event_id(parent.event_id)
                    .post_thread_id(post_thread_id)
                    .maybe_content(parent.content.djot_content.as_deref())
                    .reply_count(parent.reply_count)
                    .timestamp(parent.ts)
                    .ro(state.ro_mode(session.session_token()))
                    .call().await?)
            }
        }

        // Show replies
        @for comment in &comments {
            @if let Some(djot_content) = comment.content.djot_content.as_ref() {
                div ."o-mainBarTimeline__item" style="margin-left: 1rem;" {
                    (state.render_post_context(
                        &client_ref,
                        comment.author
                        ).event_id(comment.event_id)
                        .post_thread_id(post_thread_id)
                        .content(djot_content)
                        .reply_count(comment.reply_count)
                        .timestamp(comment.ts)
                        .ro(state.ro_mode(session.session_token()))
                        .call().await?)
                }
            }
        }
    };

    Ok(Maud(
        state
            .render_nojs_full_page(&session, "Replies", body)
            .await?,
    ))
}

#[bon::bon]
impl UiState {
    #[builder]
    pub(crate) async fn timeline_common_navbar(
        &self,
        session: &UserSession,
        /// If true, the new post form is hidden (e.g., on shoutbox page which
        /// has its own input)
        #[builder(default)]
        hide_new_post_form: bool,
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

                @if !hide_new_post_form {
                    (self.new_post_form(None, ro_mode, Some(user_id)))
                }
            }
        })
    }

    async fn handle_get_updates(
        &self,
        mut ws: WebSocket,
        user: &UserSession,
        initial_pending: PendingCounts,
        on_shoutbox: bool,
    ) -> RequestResult<()> {
        let client = self.client(user.id()).await?;
        let client_ref = client.client_ref()?;
        let self_id = client_ref.rostra_id();
        let mut new_posts = client_ref.new_posts_subscribe();
        let mut new_shoutbox = client_ref.new_shoutbox_subscribe();

        let followees: HashMap<RostraId, PersonaSelector> = client_ref
            .db()
            .get_followees(self_id)
            .await
            .into_iter()
            .collect();

        let mut followees_count = initial_pending.followees as u64;
        let mut network_count = initial_pending.network as u64;
        let mut notifications_count = initial_pending.notifications as u64;
        let mut shoutbox_count = initial_pending.shoutbox as u64;

        loop {
            tokio::select! {
                result = new_posts.recv() => {
                    let (event_content, social_post) = match result {
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
                }
                result = new_shoutbox.recv() => {
                    let (event_content, shoutbox_content) = match result {
                        Ok(event_content) => event_content,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            continue;
                        }
                    };
                    let author = event_content.event.event.author;

                    // Only count shoutbox posts if not on shoutbox page
                    if !on_shoutbox && author != self_id {
                        shoutbox_count += 1;
                    }

                    // Send the rendered shout for live updates (only if on shoutbox page)
                    if on_shoutbox {
                        let shout_html = self
                            .render_shoutbox_post_live(&client_ref, self_id, author, &shoutbox_content)
                            .await
                            .into_string();
                        let _ = ws.send(shout_html.into()).await;
                    }
                }
            }

            let badge_html = self
                .render_tab_badges(
                    followees_count,
                    network_count,
                    notifications_count,
                    shoutbox_count,
                )
                .into_string();
            let _ = ws.send(badge_html.into()).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    fn render_tab_badges(
        &self,
        followees: u64,
        network: u64,
        notifications: u64,
        shoutbox: u64,
    ) -> Markup {
        let dispatch = format!(
            "$dispatch('badges:updated', {{ followees: {followees}, network: {network}, notifications: {notifications}, shoutbox: {shoutbox} }})"
        );
        html! {
            div x-init=(dispatch) {}
        }
    }

    /// Render a shoutbox post for live WebSocket updates.
    /// Returns HTML that will be appended to #shoutbox-posts via x-merge.
    async fn render_shoutbox_post_live(
        &self,
        client: &ClientRef<'_>,
        self_id: RostraId,
        author: RostraId,
        content: &content_kind::Shoutbox,
    ) -> Markup {
        let profile = self.get_social_profile(author, client).await;
        let rendered_content = self
            .render_content(client, author, &content.djot_content)
            .await;

        // Get the actual latest cursor from the database
        let latest_cursor = client.db().get_latest_shoutbox_received_at_cursor().await;

        // Cookie name and value for updating "last seen" when on shoutbox page
        let cookie_name = format!("{}-shoutbox-last-seen", self_id.to_short());
        let cookie_value = latest_cursor
            .map(|c| format!(r#"{{"ts":{},"seq":{}}}"#, u64::from(c.ts), c.seq))
            .unwrap_or_default();

        // WebSocket handler supports x-merge="append" for appending children to target
        html! {
            div id="shoutbox-posts" x-merge="append" {
                div ."o-shoutbox__post -new"
                    x-autofocus
                    x-init=(format!(r#"document.cookie = '{cookie_name}=' + encodeURIComponent('{cookie_value}') + '; path=/; max-age=31536000';"#))
                {
                    img ."o-shoutbox__avatar u-userImage"
                        src=(self.avatar_url(author))
                        alt="Avatar"
                        {}
                    div ."o-shoutbox__postBody" {
                        div ."o-shoutbox__postMeta" {
                            a ."o-shoutbox__author" href=(format!("/profile/{}", author)) {
                                (profile.display_name)
                            }
                            span ."o-shoutbox__timestamp" { "just now" }
                        }
                        div ."o-shoutbox__postContent" {
                            (rendered_content)
                        }
                    }
                }
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
        let (pending_counts, debug_info) = self
            .handle_notification_cookies(&client_ref, pagination.is_some(), cookies, mode)
            .await?;

        let timeline = self
            .render_main_bar_timeline(session, mode)
            .maybe_pagination(pagination)
            .pending_counts(pending_counts)
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
    ) -> RequestResult<(PendingCounts, super::debug::NotificationDebugInfo)> {
        use super::debug::NotificationDebugInfo;

        // If this is a non-first page, we don't need to do anything
        if is_paginated {
            return Ok((PendingCounts::default(), NotificationDebugInfo::default()));
        }

        match mode {
            TimelineMode::Profile(_) | TimelineMode::ProfileSingle(_, _) => {
                Ok((PendingCounts::default(), NotificationDebugInfo::default()))
            }
            TimelineMode::Followees | TimelineMode::Network | TimelineMode::Notifications => {
                let rostra_id = client.rostra_id();
                let latest_cursor = client
                    .db()
                    .get_latest_social_post_received_at_cursor()
                    .await;

                // Save the cursor for the current tab
                if let Some(cursor) = latest_cursor {
                    match mode {
                        TimelineMode::Followees => {
                            cookies.save_followees_last_seen(rostra_id, cursor)
                        }
                        TimelineMode::Network => cookies.save_network_last_seen(rostra_id, cursor),
                        TimelineMode::Notifications => {
                            cookies.save_notifications_last_seen(rostra_id, cursor)
                        }
                        _ => {}
                    }
                }

                // Count pending for all tabs (except the current one which is now 0)
                let pending = self.count_pending_for_tabs(client, cookies, mode).await;

                Ok((
                    pending,
                    NotificationDebugInfo::for_save(mode, latest_cursor),
                ))
            }
        }
    }

    async fn count_pending_for_tabs(
        &self,
        client: &ClientRef<'_>,
        cookies: &Cookies,
        current_mode: TimelineMode,
    ) -> PendingCounts {
        let rostra_id = client.rostra_id();

        let followees_count = if current_mode == TimelineMode::Followees {
            0
        } else {
            self.count_pending_for_tab(
                client,
                cookies.get_followees_last_seen(rostra_id),
                TimelineMode::Followees,
            )
            .await
        };

        let network_count = if current_mode == TimelineMode::Network {
            0
        } else {
            self.count_pending_for_tab(
                client,
                cookies.get_network_last_seen(rostra_id),
                TimelineMode::Network,
            )
            .await
        };

        let notifications_count = if current_mode == TimelineMode::Notifications {
            0
        } else {
            self.count_pending_for_tab(
                client,
                cookies.get_notifications_last_seen(rostra_id),
                TimelineMode::Notifications,
            )
            .await
        };

        // Count pending shoutbox posts
        let shoutbox_count = client
            .db()
            .count_shoutbox_posts_since(cookies.get_shoutbox_last_seen(rostra_id), 10)
            .await;

        PendingCounts {
            followees: followees_count,
            network: network_count,
            notifications: notifications_count,
            shoutbox: shoutbox_count,
        }
    }

    async fn count_pending_for_tab(
        &self,
        client: &ClientRef<'_>,
        cookie_cursor: Option<ReceivedAtPaginationCursor>,
        mode: TimelineMode,
    ) -> usize {
        let start_cursor = cookie_cursor.map(|c| c.next());
        let (posts, _) = client
            .db()
            .paginate_social_posts_by_received_at(start_cursor, 10, mode.to_filter_fn(client).await)
            .await;
        posts.len()
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
        pending_counts: PendingCounts,
        debug_info: Option<super::debug::NotificationDebugInfo>,
    ) -> RequestResult<Markup> {
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

        let ws_url = format!(
            "websocket('/updates?followees={f}&network={n}&notifications={no}&shoutbox={s}')",
            f = pending_counts.followees,
            n = pending_counts.network,
            no = pending_counts.notifications,
            s = pending_counts.shoutbox
        );
        let badge_counts = format!(
            "badgeCounts({{ followees: {}, network: {}, notifications: {}, shoutbox: {} }})",
            pending_counts.followees,
            pending_counts.network,
            pending_counts.notifications,
            pending_counts.shoutbox
        );

        Ok(html! {
            div ."o-mainBarTimeline"
                x-data=(ws_url)
            {
                div ."o-mainBarTimeline__tabs"
                    x-data=(badge_counts)
                    "@badges:updated.window"="onUpdate($event.detail)"
                {
                    a ."o-mainBarTimeline__back" href="/" onclick="history.back(); return false;" { "<" }

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
                            "Following"
                            span ."o-mainBarTimeline__newCount" x-text="formatCount(followees)" {}
                        }
                        a ."o-mainBarTimeline__network"
                            ."-active"[mode.is_network()]
                            href=(TimelineMode::Network.to_path())
                        {
                            "Network"
                            span ."o-mainBarTimeline__newCount" x-text="formatCount(network)" {}
                        }
                        a ."o-mainBarTimeline__notifications"
                            ."-active"[mode.is_notifications()]
                            href=(TimelineMode::Notifications.to_path())
                            ":class"="{ '-pending': notifications > 0 }"
                        {
                            "Notifications"
                            span ."o-mainBarTimeline__pendingNotifications" x-text="formatCount(notifications)" {}
                        }
                        a ."o-mainBarTimeline__shoutbox"
                            href="/shoutbox"
                            ":class"="{ '-pending': shoutbox > 0 }"
                        {
                            "Shoutbox"
                            span ."o-mainBarTimeline__newCount" x-text="formatCount(shoutbox)" {}
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
