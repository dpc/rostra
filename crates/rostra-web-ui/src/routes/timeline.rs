use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Form;
use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_client::ClientRef;
use rostra_client_db::IdSocialProfileRecord;
use rostra_client_db::social::{EventPaginationCursor, SocialPostRecord};
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
use crate::util::extractors::AjaxRequest;
use crate::{LOG_TARGET, SharedState, UiState};

#[derive(Deserialize)]
pub struct TimelinePaginationInput {
    pub ts: Option<Timestamp>,
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
            .map(|event_id| EventPaginationCursor { ts, event_id })
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
            .map(|event_id| EventPaginationCursor { ts, event_id })
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
        form.event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
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

pub async fn get_updates(
    state: State<SharedState>,
    ws: WebSocketUpgrade,
    session: UserSession,
) -> impl IntoResponse {
    ws.on_upgrade(|ws| async move {
        let _ = state
            .handle_get_updates(ws, &session)
            .await
            .inspect_err(|err| {
                debug!(target: LOG_TARGET, err=%err.fmt_compact(), "WS handler failed");
            });
    })
}

pub async fn get_post_comments(
    state: State<SharedState>,
    session: UserSession,
    Path(id): Path<ShortEventId>,
) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.render_post_comments(id, &session).await?))
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

        Ok(html! {
            nav ."o-navBar"
                x-data="websocket('/ui/updates')"
            {
                (self.render_top_nav())

                div ."o-navBar__profileSummary" {
                    (self.render_self_profile_summary(session, session.ro_mode()).await?)
                }

                (self.new_post_form(None, session.ro_mode(), Some(user_id)))
            }
        })
    }

    async fn handle_get_updates(&self, mut ws: WebSocket, user: &UserSession) -> RequestResult<()> {
        let client = self.client(user.id()).await?;
        let self_id = client.client_ref()?.rostra_id();
        let mut new_posts = client.client_ref()?.new_posts_subscribe();

        let mut count = 0;

        loop {
            let (event_content, _social_post) = match new_posts.recv().await {
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
            if event_content.event.event.author == self_id {
                continue;
            }
            count += 1;
            let _ = ws
                .send(
                    html! {
                        (self.render_new_posts_alert(true, count))
                    }
                    .into_string()
                    .into(),
                )
                .await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    pub(crate) async fn render_timeline_page(
        &self,
        navbar: Markup,
        pagination: Option<EventPaginationCursor>,
        session: &UserSession,
        cookies: &mut Cookies,
        mode: TimelineMode,
        is_ajax_request: bool,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;
        let pending_notifications = self
            .handle_notification_cookies(&client_ref, pagination, cookies, mode)
            .await?;

        let timeline = self
            .render_main_bar_timeline(session, mode)
            .maybe_pagination(pagination)
            .maybe_pending_notifications(pending_notifications)
            .call()
            .await?;

        if is_ajax_request {
            return Ok(timeline);
        }
        // Otherwise return the full page
        let page_layout = self.render_page_layout(
            navbar,
            html! {
                (self.render_new_posts_alert(false, 0))
                (timeline)
            },
        );

        let content = html! {
            (page_layout)

            // Dialog containers for timeline interactions
            div id="preview-dialog" ."o-previewDialog" x-sync {}
            div id="media-list" ."o-mediaList" x-sync {}
            div id="ajax-scripts" style="display: none;" {}
            div ."o-followDialog" {
                div id="follow-dialog-content" {}
            }

            // Initialize emoji picker once (outside form to avoid re-execution)
            script type="module" {
                (PreEscaped(r#"
                    import { Picker } from '/assets/libs/emoji-picker-element/index.js';
                    import textFieldEdit from '/assets/libs/text-field-edit/index.js';

                    const emojiPicker = document.querySelector('emoji-picker');
                    if (emojiPicker) {
                        emojiPicker.addEventListener('emoji-click', e => {
                            const textarea = document.querySelector('.m-newPostForm__content');
                            if (textarea) {
                                textFieldEdit.insert(textarea, e.detail.unicode);
                            }
                        });
                    }
                "#))
            }

            // Initialize mention autocomplete once (outside form to avoid re-execution)
            script {
                (PreEscaped(r#"
                    document.addEventListener('alpine:init', () => {
                        Alpine.data('mentionAutocomplete', () => ({
                            query: '',
                            results: [],
                            selectedIndex: 0,
                            showDropdown: false,
                            debounceTimer: null,

                            handleMentionInput(event) {
                                const textarea = event.target;
                                const cursorPos = textarea.selectionStart;
                                const textBeforeCursor = textarea.value.substring(0, cursorPos);

                                const atMatch = textBeforeCursor.match(/@(\w*)$/);

                                if (atMatch) {
                                    this.query = atMatch[1];
                                    this.showDropdown = true;
                                    this.searchProfiles();
                                } else {
                                    this.showDropdown = false;
                                }
                            },

                            searchProfiles() {
                                clearTimeout(this.debounceTimer);
                                this.debounceTimer = setTimeout(async () => {
                                    try {
                                        const response = await fetch(`/ui/search/profiles?q=${encodeURIComponent(this.query)}`);
                                        this.results = await response.json();
                                        this.selectedIndex = 0;
                                    } catch (error) {
                                        console.error('Failed to search profiles:', error);
                                        this.results = [];
                                    }
                                }, 300);
                            },

                            selectProfile(profile) {
                                const textarea = this.$root.querySelector('textarea');
                                if (!textarea) {
                                    console.error('Textarea not found');
                                    return;
                                }

                                const cursorPos = textarea.selectionStart;
                                const textBeforeCursor = textarea.value.substring(0, cursorPos);
                                const textAfterCursor = textarea.value.substring(cursorPos);

                                const atPos = textBeforeCursor.lastIndexOf('@');

                                const newText =
                                    textBeforeCursor.substring(0, atPos) +
                                    `<rostra:${profile.rostra_id}>` +
                                    textAfterCursor;

                                textarea.value = newText;

                                const newCursorPos = atPos + `<rostra:${profile.rostra_id}>`.length;
                                textarea.setSelectionRange(newCursorPos, newCursorPos);

                                textarea.dispatchEvent(new Event('input', { bubbles: true }));

                                this.showDropdown = false;
                            },

                            handleKeydown(event) {
                                if (!this.showDropdown) return;

                                if (event.key === 'ArrowDown' || (event.key === 'Tab' && !event.shiftKey)) {
                                    event.preventDefault();
                                    if (this.results.length > 0) {
                                        this.selectedIndex = Math.min(this.selectedIndex + 1, this.results.length - 1);
                                    }
                                } else if (event.key === 'ArrowUp' || (event.key === 'Tab' && event.shiftKey)) {
                                    event.preventDefault();
                                    if (this.results.length > 0) {
                                        this.selectedIndex = Math.max(this.selectedIndex - 1, 0);
                                    }
                                } else if (event.key === 'Enter' && this.results.length > 0) {
                                    event.preventDefault();
                                    this.selectProfile(this.results[this.selectedIndex]);
                                } else if (event.key === 'Escape') {
                                    event.preventDefault();
                                    this.showDropdown = false;
                                }
                            }
                        }));
                    });
                "#))
            }

            // Initialize insertMediaSyntax function once
            script {
                (PreEscaped(r#"
                    window.insertMediaSyntax = function(eventId) {
                        const textarea = document.querySelector('.m-newPostForm__content');
                        const syntax = '![](rostra-media:' + eventId + ')';

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

        self.render_html_page("Rostra", content).await
    }

    pub(crate) async fn handle_notification_cookies(
        &self,
        client: &ClientRef<'_>,
        pagination: Option<EventPaginationCursor>,
        cookies: &mut Cookies,
        mode: TimelineMode,
    ) -> RequestResult<Option<usize>> {
        // If this is a non-first page, we don't need to do anything
        if pagination.is_some() {
            return Ok(None);
        }

        match mode {
            TimelineMode::Profile(_) | TimelineMode::ProfileSingle(_, _) => {
                // We're not displaying notifications on profile timelines
                Ok(None)
            }
            TimelineMode::Notifications => {
                if let Some(latest_event) = client
                    .db()
                    .paginate_social_posts_rev(None, 1, |_| true)
                    .await
                    .0
                    .into_iter()
                    .next()
                {
                    cookies.save_last_seen(
                        client.rostra_id(),
                        EventPaginationCursor {
                            ts: latest_event.ts,
                            event_id: latest_event.event_id,
                        },
                    );
                }
                Ok(None)
            }
            TimelineMode::Followees | TimelineMode::Network => {
                let pending_len = client
                    .db()
                    .paginate_social_posts(
                        cookies.get_last_seen(client.rostra_id()),
                        10,
                        TimelineMode::Notifications.to_filter_fn(client).await,
                    )
                    .await
                    .0
                    .len();

                Ok(Some(pending_len))
            }
        }
    }

    pub async fn render_post_comments(
        &self,
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
            div ."m-postView__comments"
                id=(format!("post-comments-{}", post_id))
            {
                @for comment in comments {
                    @if let Some(djot_content) = comment.content.djot_content.as_ref() {
                        div ."o-postOverview__commentsItem" {
                            (self.render_post_context(
                                &client_ref,
                                comment.author
                                ).event_id(comment.event_id)
                                .content(djot_content)
                                .reply_count(comment.reply_count)
                                .timestamp(comment.ts)
                                .ro(session.ro_mode())
                                .call().await?)
                        }
                    }
                }

                (re_typeset_mathjax())
            }
        })
    }

    pub fn render_new_posts_alert(&self, visible: bool, count: u64) -> Markup {
        html! {
            // TODO: Update new posts alert - need Alpine.js event or different approach
            // (alpine-ajax doesn't support x-swap-oob)
            a ."o-mainBar__newPostsAlert"
                ."-hidden"[!visible]
                 href="/ui"
            {
                (if count == 0 {
                    "No new posts available".to_string()
                } else if count == 1 {
                    "New post available".to_string()
                } else {
                    format!("{count} new posts available.")
                })
            }
        }
    }

    #[builder]
    pub(crate) async fn render_main_bar_timeline(
        &self,
        #[builder(start_fn)] session: &UserSession,
        #[builder(start_fn)] mode: TimelineMode,
        pagination: Option<EventPaginationCursor>,
        pending_notifications: Option<usize>,
    ) -> RequestResult<Markup> {
        let pending_notifications = pending_notifications.unwrap_or_default();
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
            div ."o-mainBarTimeline" "x-data"="{}" {
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
                        { "Followees" }
                        a  ."o-mainBarTimeline__network"
                            ."-active"[mode.is_network()]
                            href=(TimelineMode::Network.to_path())
                        { "Network" }
                        a ."o-mainBarTimeline__notifications"
                            ."-active"[mode.is_notifications()]
                            href=(TimelineMode::Notifications.to_path())
                            ."-pending"[0 < pending_notifications]
                        {
                            "Notifications"
                            @if 9 < pending_notifications {
                                span ."o-mainBarTimeline__pendingNotifications" { "(9+)" }
                            } @else if 0 < pending_notifications {
                                span ."o-mainBarTimeline__pendingNotifications" { "("(pending_notifications) ")" }
                            }
                        }
                    }
                }
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
                div id="post-preview" ."o-mainBarTimeline__item -preview -empty" x-sync { }
                div id="timeline-posts" x-merge="append" {
                    @for post in &filtered_posts {
                        @if let Some(djot_content) = post.content.djot_content.as_ref() {
                            div ."o-mainBarTimeline__item"
                            ."-reply"[post.reply_to.is_some()]
                            ."-post"[post.reply_to.is_none()]
                            {
                                (self.render_post_context(
                                    &client_ref,
                                    post.author,
                                    ).maybe_persona_display_name(
                                    author_personas.get(&(post.author, post.content.persona)).map(AsRef::as_ref)

                                        )
                                        .maybe_reply_to(
                                        post.reply_to
                                            .map(|reply_to| (reply_to.rostra_id(), reply_to.event_id(), parents.get(&reply_to.event_id().to_short())))
                                        )
                                        .event_id(post.event_id)
                                        .content(djot_content)
                                        .reply_count(post.reply_count)
                                        .timestamp(post.ts)
                                        .ro(session.ro_mode())
                                        .call().await?)
                            }
                        }
                    }
                }
                @if let Some(cursor) = cursor {
                    @let href = format!("{}?ts={}&event_id={}", mode.to_path(), cursor.ts, cursor.event_id);
                    div id="load-more-posts" ."o-mainBarTimeline__rest -empty"
                        "data-url"=(href)
                        "x-intersect.once"="$ajax($el.dataset.url, { targets: ['timeline-posts', 'load-more-posts'] })"
                    { }
                } @else {
                    div id="load-more-posts" ."o-mainBarTimeline__rest -empty" {}
                }
            }
            script {
                (PreEscaped(r#"
                    // Fix: Prevent unwanted scroll-to-top during infinite scroll
                    let savedScrollTop = 0;
                    let isLoadingMore = false;

                    document.addEventListener('ajax:before', () => {
                        savedScrollTop = document.body.scrollTop;
                        isLoadingMore = true;
                    });

                    document.body.addEventListener('scroll', () => {
                        // Detect and prevent unwanted scroll-to-top during infinite scroll load
                        if (isLoadingMore && savedScrollTop > 100 && document.body.scrollTop < 100) {
                            document.body.scrollTop = savedScrollTop;
                            isLoadingMore = false;
                            savedScrollTop = 0;
                        }
                    });
                "#))
            }
            // TODO: we probably need it, but I don't know why :D
            // script {
            //     (PreEscaped(r#"
            //         document.querySelector('.o-mainBarTimeline')
            //             .classList.toggle(
            //                 '-hideReplies',
            //                 !document.querySelector('.o-mainBarTimeline__showReplies').checked
            //             );
            //     "#))
            // }
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
                    href={"/ui/profile/"(id)}
                {
                    (display_name)
                }
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
            TimelineMode::Followees => "/ui/followees".to_string(),
            TimelineMode::Network => "/ui/network".to_string(),
            TimelineMode::Notifications => "/ui/notifications".to_string(),
            TimelineMode::Profile(rostra_id) => format!("/ui/profile/{rostra_id}"),
            TimelineMode::ProfileSingle(rostra_id, _) => format!("/ui/profile/{rostra_id}"),
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
        pagination: Option<EventPaginationCursor>,
    ) -> (
        Vec<SocialPostRecord<SocialPost>>,
        Option<EventPaginationCursor>,
    ) {
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

            client
                .db()
                .paginate_social_posts_rev(pagination, 20, filter_fn)
                .await
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
            TimelineMode::Notifications => Box::new(move |post| {
                post.author != self_id
                    && post.reply_to.map(|ext_id| ext_id.rostra_id()) == Some(self_id)
            }),
            TimelineMode::Profile(rostra_id) => Box::new(move |post| post.author == rostra_id),
            TimelineMode::ProfileSingle(_, _) => {
                warn!(target: LOG_TARGET, "Should not be here");
                Box::new(move |_post| false)
            }
        }
    }
}
