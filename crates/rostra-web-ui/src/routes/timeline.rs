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
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_util_error::FmtCompact as _;
use serde::Deserialize;
use tower_cookies::Cookies;
use tracing::debug;

use super::super::error::RequestResult;
use super::Maud;
use super::cookies::CookiesExt as _;
use super::unlock::session::{RoMode, UserSession};
use crate::html_utils::re_typeset_mathjax;
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
            )
            .await?,
    ))
}

pub async fn get_network(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
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
            )
            .await?,
    ))
}

pub async fn get_notifications(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
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
    async fn timeline_common_navbar(&self, session: &UserSession) -> RequestResult<Markup> {
        Ok(html! {
            nav ."o-navBar"
                hx-ext="ws"
                ws-connect="/ui/updates"
                // doesn't work, gets lowercased, wait for https://github.com/lambda-fairy/maud/pull/445
                // hx-on:htmx:wsError="console.log(JSON.stringify(event))"
            {
                div ."o-navBar__list" {
                    span ."o-navBar__header" { "Rostra:" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra/discussions" { "Support" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra/wiki" { "Wiki" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra" { "Github" }
                }

                div ."o-navBar__profileSummary" {
                    (self.render_self_profile_summary(session, session.ro_mode()).await?)
                }

                (self.render_add_followee_form(None))

                (self.new_post_form(None, session.ro_mode()))
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
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;
        let pending_notifications = self
            .handle_notification_cookies(&client_ref, pagination, cookies, mode)
            .await?;

        let content = html! {

            (navbar)

            main ."o-mainBar" {
                (self.render_new_posts_alert(false, 0))
                (self.render_main_bar_timeline(session, mode)
                    .maybe_pagination(pagination)
                    .maybe_pending_notifications(pending_notifications)
                    .call()
                    .await?)


            }
            div .o-previewDialog {}
            div .o-followDialog {}
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
            TimelineMode::Profile(_) => {
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
            div ."m-postOverview__comments" {
                @for comment in comments {
                    @if let Some(djot_content) = comment.content.djot_content.as_ref() {
                        div ."o-postOverview__commentsItem" {
                            (self.post_overview(
                                &client_ref,
                                comment.author
                                ).event_id(comment.event_id)
                                .content(djot_content)
                                .reply_count(comment.reply_count)
                                .ro(session.ro_mode())
                                .is_comment(true)
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
            a ."o-mainBar__newPostsAlert"
                ."-hidden"[!visible]
                hx-swap-oob=[visible.then_some("outerHTML: .o-mainBar__newPostsAlert")]
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

        let filter_fn = mode.to_filter_fn(&client_ref).await;

        let (filtered_posts, cursor) = client_ref
            .db()
            .paginate_social_posts_rev(pagination, 30, filter_fn)
            .await;

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
            div ."o-mainBarTimeline" {
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
                div ."o-mainBarTimeline__item -preview -empty" { }
                @for post in &filtered_posts {
                    @if let Some(djot_content) = post.content.djot_content.as_ref() {
                        div ."o-mainBarTimeline__item"
                        ."-reply"[post.reply_to.is_some()]
                        ."-post"[post.reply_to.is_none()]
                        {
                            (self.post_overview(
                                &client_ref,
                                post.author,
                                ).maybe_persona_display_name(
                                author_personas.get(&(post.author, post.content.persona)).map(AsRef::as_ref)

                                    )
                                    .maybe_reply_to(
                                    post.reply_to
                                        .map(|reply_to| (reply_to.rostra_id(), parents.get(&reply_to.event_id().to_short())))
                                    )
                                    .event_id(post.event_id)
                                    .content(djot_content)
                                    .reply_count(post.reply_count)
                                    .ro(session.ro_mode())
                                    .call().await?)
                        }
                    }
                }
                @if let Some(cursor) = cursor {
                    div ."o-mainBarTimeline__rest -empty"
                        hx-get=(
                            format!("{}?ts={}&event_id={}",
                                mode.to_path(),
                                cursor.ts,
                                cursor.event_id)
                        )
                        hx-select=".o-mainBarTimeline__item, .o-mainBarTimeline__rest, script.mathjax"
                        hx-trigger="intersect once, threshold:0.5"
                        hx-swap="outerHTML"
                    { }
                }
            }
            script {
                (PreEscaped(r#"
                    document.querySelector('.o-mainBarTimeline')
                        .classList.toggle(
                            '-hideReplies',
                            !document.querySelector('.o-mainBarTimeline__showReplies').checked
                        );
                "#))
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub async fn post_overview(
        &self,
        #[builder(start_fn)] client: &ClientRef<'_>,
        #[builder(start_fn)] author: RostraId,
        persona_display_name: Option<&str>,
        reply_to: Option<(RostraId, Option<&SocialPostRecord<SocialPost>>)>,
        event_id: Option<ShortEventId>,
        content: Option<&str>,
        reply_count: Option<u64>,
        ro: RoMode,
        // Render the post including a comment, right away
        comment: Option<Markup>,
        // Is the post loaded as a comment to an existing post (already being
        // displayed)
        #[builder(default = false)] is_comment: bool,
    ) -> RequestResult<Markup> {
        let external_event_id = event_id.map(|e| ExternalEventId::new(author, e));
        let user_profile = self.get_social_profile_opt(author, client).await;

        // Note: we are actually not doing pagiantion, and just ignore
        // everything after first page
        let (reactions, _) = if let Some(event_id) = event_id {
            client
                .db()
                .paginate_social_post_reactions_rev(event_id, None, 1000)
                .await
        } else {
            (vec![], None)
        };

        let mut reaction_social_profiles: HashMap<RostraId, IdSocialProfileRecord> = HashMap::new();

        for reaction_author in reactions
            .iter()
            .map(|reaction| reaction.author)
            // collect to deduplicate
            .collect::<HashSet<_>>()
        {
            // TODO: make a batched request for all profiles in one go
            if let Some(reaction_user_profile) =
                self.get_social_profile_opt(reaction_author, client).await
            {
                // HashSet above must have deduped it
                assert!(
                    reaction_social_profiles
                        .insert(reaction_author, reaction_user_profile)
                        .is_none()
                );
            }
        }

        let reactions_html = html! {
            @for reaction in reactions {
                @if let Some(reaction_text) = reaction.content.get_reaction() {

                    span .m-postOverview__reaction
                        title=(
                            format!("by {}",
                                reaction_social_profiles.get(&reaction.author)
                                    .map(|r| r.display_name.clone())
                                    .unwrap_or_else(|| reaction.author.to_string())
                            )
                        )
                    {
                        (reaction_text)
                    }
                }
            }
        };

        let post_content_rendered = if let Some(content) = content.as_ref() {
            Some(self.render_content(client, content).await)
        } else {
            None
        };

        let post_main = html! {
            div ."m-postOverview__main"
            {
                img ."m-postOverview__userImage u-userImage"
                    src=(self.avatar_url(author))
                    width="32pt"
                    height="32pt"
                { }

                div ."m-postOverview__contentSide"
                    onclick=[comment.as_ref().map(|_|"this.classList.add('-expanded')" )]
                {
                    header ."m-postOverview__header" {
                        span ."m-postOverview__userHandle" {
                            (self.render_user_handle(event_id, author, user_profile.as_ref()))
                        }
                        @if let Some(persona_display_name) = persona_display_name {
                            span ."m-postOverview__personaDisplayName" {
                                (format!("({})", persona_display_name))
                            }
                        }
                    }

                    div ."m-postOverview__content"
                     ."-missing"[post_content_rendered.is_none()]
                     ."-present"[post_content_rendered.is_some()]
                    {
                        p {
                            @if let Some(post_content_rendered) = post_content_rendered {
                                (post_content_rendered)
                            } @else {
                                    "Post missing"
                            }
                        }
                    }
                }

            }
        };

        let button_bar = html! {
            @if let Some(ext_event_id) = external_event_id {
                div ."m-postOverview__buttonBar" {
                    div .m-postOverview__reactions {
                        (reactions_html)
                    }
                    div ."m-postOverview__buttons" {
                        @if let Some(reply_count) = reply_count {
                            @if reply_count > 0 {
                                button ."m-postOverview__commentsButton u-button"
                                    hx-get={"/ui/comments/"(ext_event_id.event_id().to_short())}
                                    hx-target="next .m-postOverview__comments"
                                    hx-swap="outerHTML"
                                    hx-on::after-request="this.classList.add('u-hidden')"
                                {
                                    span ."m-postOverview__commentsButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                    @if reply_count == 1 {
                                        ("1 Reply".to_string())
                                    } @else {
                                        (format!("{} Replies", reply_count))
                                    }
                                }
                            }

                        }
                        button ."m-postOverview__replyToButton u-button"
                            disabled[ro.to_disabled()]
                            hx-get={"/ui/post/reply_to?reply_to="(ext_event_id)}
                            hx-target=".m-newPostForm__replyToLine"
                            hx-swap="outerHTML"
                        {
                            span ."m-postOverview__replyToButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                            "Reply"
                        }
                    }
                }
            }
        };

        let post_id = format!(
            "post-{}",
            event_id
                .map(|e| e.to_string())
                .unwrap_or_else(|| "preview".to_string()),
        );

        let post = html! {
            article #(post_id)
                ."m-postOverview"
                ."-response"[reply_to.is_some() || is_comment]
                ."-reply-parent"[comment.is_some()]
             {
                (post_main)

                (button_bar)

                div ."m-postOverview__comments"
                    ."-empty"[comment.is_none()]
                {
                    @if let Some(comment) = comment {
                        div ."m-postOverview__commentsItem" {
                            (comment)
                        }
                    }
                }
            }
        };

        Ok(html! {
            @if let Some((reply_to_author, reply_to_post)) = reply_to {
                @if let Some(reply_to_post) = reply_to_post {
                    @if let Some(djot_content) = reply_to_post.content.djot_content.as_ref() {
                        (Box::pin(self.post_overview(
                            client,
                            reply_to_post.author
                            )
                            .event_id(reply_to_post.event_id)
                            .content(djot_content)
                            .ro(ro)
                            .comment(post)
                            .call()
                        )
                        .await?)
                    }
                } @else {
                    (Box::pin(self.post_overview(
                        client,
                        reply_to_author,
                        )
                        .ro(ro).comment(post)
                        .call()
                    ).await?)
                }
            } @else {
                (post)
            }
        })
    }

    fn render_user_handle(
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
                    ."a-userNameHandle__displayName"
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
}

impl TimelineMode {
    fn to_path(self) -> String {
        match self {
            TimelineMode::Followees => "/ui/followees".to_string(),
            TimelineMode::Network => "/ui/network".to_string(),
            TimelineMode::Notifications => "/ui/notifications".to_string(),
            TimelineMode::Profile(rostra_id) => format!("/ui/profile/{rostra_id}"),
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
        }
    }
}
