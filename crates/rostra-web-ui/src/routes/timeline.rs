use std::collections::HashSet;
use std::time::Duration;

use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_client_db::social::{EventPaginationCursor, SocialPostRecord};
use rostra_client_db::IdSocialProfileRecord;
use rostra_core::event::{EventKind, SocialPost};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_util_error::FmtCompact as _;
use serde::Deserialize;
use tracing::debug;

use super::super::error::RequestResult;
use super::unlock::session::{RoMode, UserSession};
use super::Maud;
use crate::html_utils::re_typeset_mathjax;
use crate::{SharedState, UiState, LOG_TARGET};

#[derive(Deserialize)]
pub struct TimelinePaginationInput {
    pub ts: Option<Timestamp>,
    pub event_id: Option<ShortEventId>,
    pub user: Option<RostraId>,
}

pub async fn get(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<TimelinePaginationInput>,
) -> RequestResult<impl IntoResponse> {
    let pagination = form.ts.and_then(|ts| {
        form.event_id
            .map(|event_id| EventPaginationCursor { ts, event_id })
    });
    let navbar = html! {
        nav ."o-navBar"
            hx-ext="ws"
            ws-connect="/ui/timeline/updates"
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
                (state.render_self_profile_summary(&session, session.ro_mode()).await?)
            }

            (state.render_add_followee_form(None))

            (state.new_post_form(None, session.ro_mode()))
        }
    };
    Ok(Maud(
        state
            .render_timeline_page(navbar, pagination, &session, form.user)
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

    pub async fn render_timeline_page(
        &self,
        navbar: Markup,
        pagination: Option<EventPaginationCursor>,
        session: &UserSession,
        filter_user: Option<RostraId>,
    ) -> RequestResult<Markup> {
        let content = html! {

            (navbar)

            main ."o-mainBar" {
                (self.render_new_posts_alert(false, 0))
                (self.render_main_bar_timeline(pagination, session, filter_user).await?)
            }
            (re_typeset_mathjax())

        };

        self.render_html_page("Rostra", content).await
    }

    pub async fn render_post_comments(
        &self,
        post_id: ShortEventId,
        user: &UserSession,
    ) -> RequestResult<Markup> {
        let client = self.client(user.id()).await?;
        let client_ref = client.client_ref()?;

        let comments = self
            .client(user.id())
            .await?
            .db()?
            .paginate_social_post_comments_rev(post_id, None, 100)
            .await;

        Ok(html! {
            div ."m-postOverview__comments" {
                @for comment in comments {
                    div ."o-postOverview__commentsItem" {
                        (self.post_overview(
                            &client_ref,
                            comment.author
                            ).event_id(comment.event_id)
                            .content(&comment.content.djot_content)
                            .reply_count(comment.reply_count)
                            .ro(user.ro_mode())
                            .is_comment(true)
                            .call().await?)
                    }
                }

                // Hide the button that created us
                div hx-swap-oob={"outerHTML: #post-" (post_id) " .m-postOverview__commentsButton"} {

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

    pub async fn render_main_bar_timeline(
        &self,
        pagination: Option<EventPaginationCursor>,
        session: &UserSession,
        filter_user: Option<RostraId>,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        let filter = if let Some(filter) = filter_user {
            HashSet::from([filter])
        } else {
            client
                .db()?
                .get_followees(session.id())
                .await
                .into_iter()
                .map(|(k, _)| k)
                .collect()
        };

        let posts: Vec<_> = self
            .client(session.id())
            .await?
            .db()?
            .paginate_social_posts_rev(pagination, 20)
            .await
            .into_iter()
            .filter(|post| filter.contains(&post.author))
            .collect();

        let parents = self
            .client(session.id())
            .await?
            .db()?
            .get_posts_by_id(
                posts
                    .iter()
                    .flat_map(|post| post.reply_to.map(|ext_id| ext_id.event_id().to_short())),
            )
            .await;
        Ok(html! {
            div ."o-mainBarTimeline" {
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
                    // div {
                    //     label ."o-mainBarTimeline__hideRepliesLabel"
                    //         for="hide-replies"
                    //     { "Hide comments: " }
                    //     input ."o-mainBarTimeline__hideRepliesCheckbox"
                    //         type="checkbox" id="hide-replies" name="hide-replies"
                    //     {}
                    // }
                }
                div ."o-mainBarTimeline__item -preview -empty" { }
                @for post in &posts {
                    div ."o-mainBarTimeline__item"
                    ."-reply"[post.reply_to.is_some()]
                    {
                        (self.post_overview(
                            &client_ref,
                            post.author,
                            ).maybe_reply_to(
                                post.reply_to
                                    .map(|reply_to| (reply_to.rostra_id(), parents.get(&reply_to.event_id().to_short())))
                                )
                                .event_id(post.event_id)
                                .content(&post.content.djot_content)
                                .reply_count(post.reply_count)
                                .ro(session.ro_mode())
                                .call().await?)
                    }
                }
                @if let Some(last) = posts.last() {
                    div ."o-mainBarTimeline__rest -empty"
                        hx-get=({
                            let path = format!("/ui/timeline?ts={}&event_id={}", last.ts, last.event_id);
                            if let Some(filter_user) = filter_user {
                                format!("{path}&user={filter_user}")
                            } else {
                                path
                            }
                        })
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

        let post_content_rendered = content.map(|content| {
            PreEscaped(jotdown::html::render_to_string(
                jotdown::Parser::new(content).map(|e| match e {
                    jotdown::Event::Start(jotdown::Container::RawBlock { format }, attrs)
                        if format == "html" =>
                    {
                        jotdown::Event::Start(
                            jotdown::Container::CodeBlock { language: format },
                            attrs,
                        )
                    }
                    jotdown::Event::End(jotdown::Container::RawBlock { format })
                        if format == "html" =>
                    {
                        jotdown::Event::End(jotdown::Container::CodeBlock { language: format })
                    }
                    e => e,
                }),
            ))
        });

        let post_main = html! {
            div ."m-postOverview__main" {
                img ."m-postOverview__userImage u-userImage"
                    src=(self.avatar_url(author))
                    width="32pt"
                    height="32pt"
                    { }

                div ."m-postOverview__contentSide" {
                    header ."m-postOverview__header" {
                        (self.render_user_handle(event_id, author, user_profile.as_ref()))
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
                    @if let Some(reply_count) = reply_count {
                        @if reply_count > 0 {
                            button ."m-postOverview__commentsButton u-button"
                                hx-get={"/ui/timeline/comments/"(ext_event_id.event_id().to_short())}
                                hx-target="next .m-postOverview__comments"
                                hx-swap="outerHTML"
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
                    (Box::pin(self.post_overview(
                        client,
                        reply_to_post.author
                        )
                        .event_id(reply_to_post.event_id)
                        .content(&reply_to_post.content.djot_content)
                        .ro(ro)
                        .comment(post)
                        .call()
                    )
                    .await?)
                } @else {
                    (Box::pin(self.post_overview(
                        client,
                        reply_to_author,
                        )
                        .ro( ro).comment(post)
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
