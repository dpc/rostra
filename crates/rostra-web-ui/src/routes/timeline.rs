use std::future::pending;
use std::str::FromStr as _;
use std::time::Duration;

use axum::extract::ws::WebSocket;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use maud::{html, Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::event::EventKind;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};
use rostra_util_error::FmtCompact as _;
use snafu::ResultExt as _;
use tracing::debug;

use super::super::error::RequestResult;
use super::unlock::session::{RoMode, UserSession};
use super::Maud;
use crate::error::{InvalidDataSnafu, UserSnafu};
use crate::html_utils::re_typeset_mathjax;
use crate::{SharedState, UiState, LOG_TARGET};

pub async fn get(
    state: State<SharedState>,
    _user: UserSession,
    session: UserSession,
) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.render_timeline_page(&session).await?))
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
    // TODO: seems like using `[u8;16]` in path does not work as expected
    Path(id): Path<String>,
) -> RequestResult<impl IntoResponse> {
    let id = ShortEventId::from_str(&id)
        .map_err(|_| InvalidDataSnafu.build())
        .context(UserSnafu)?;
    Ok(Maud(state.render_post_comments(id, &session).await?))
}

impl UiState {
    async fn handle_get_updates(&self, mut ws: WebSocket, user: &UserSession) -> RequestResult<()> {
        let client = self.client(user.id()).await?;
        let self_id = client.client_ref()?.rostra_id();
        let Some(mut new_posts) = client.client_ref()?.new_posts_subscribe() else {
            pending::<()>().await;
            return Ok(());
        };

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

    pub async fn render_timeline_page(&self, user: &UserSession) -> RequestResult<Markup> {
        let content = html! {
            nav ."o-navBar" hx-ext="ws" ws-connect="/ui/timeline/updates" {

                div ."o-navBar__selfAccount" {
                    (self.render_self_profile_summary(user, user.ro_mode()).await?)
                }

                (self.new_post_form(None, user.ro_mode()))

                (self.render_add_followee_form(None))

                div ."o-navBar__list" {
                    span ."o-navBar__header" { "Rostra:" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra/discussions" { "Support" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra/wiki" { "Wiki" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra" { "Github" }
                }
            }

            main ."o-mainBar" {
                (self.render_new_posts_alert(false, 0))
                (self.main_bar_timeline(user).await?)
            }

            nav ."o-sideBar" {

            }
        };

        self.render_html_page("You're Rostra!", content).await
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
            .storage()??
            .paginate_social_post_comments_rev(post_id, None, 100)
            .await;

        Ok(html! {
            div ."m-postOverview__comments" {
                @for comment in comments {
                    div ."o-postOverview__commentsItem" {
                        (self.post_overview(
                            &client_ref,
                            comment.author,
                            Some(comment.event_id),
                            &comment.content.djot_content,
                            comment.reply_count,
                            user.ro_mode()
                        ).await?)
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

    pub async fn main_bar_timeline(&self, user: &UserSession) -> RequestResult<Markup> {
        let client = self.client(user.id()).await?;
        let client_ref = client.client_ref()?;

        let posts = self
            .client(user.id())
            .await?
            .storage()??
            .paginate_social_posts_rev(None, 100)
            .await;
        Ok(html! {
            div ."o-mainBarTimeline" {
                div ."o-mainBarTimeline__item -preview -empty" { }
                @for post in posts {
                    div ."o-mainBarTimeline__item" {
                        (self.post_overview(
                            &client_ref,
                            post.author,
                            Some(post.event_id),
                            &post.content.djot_content,
                            post.reply_count,
                            user.ro_mode()
                        ).await?)
                    }
                }
            }
        })
    }

    pub async fn post_overview(
        &self,
        client: &ClientRef<'_>,
        author: RostraId,
        event_id: Option<ShortEventId>,
        content: &str,
        reply_count: u64,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        let external_event_id = event_id.map(|e| ExternalEventId::new(author, e));
        let user_profile = self.get_social_profile(author, client).await?;

        let post_content_rendered = PreEscaped(jotdown::html::render_to_string(
            jotdown::Parser::new(content).map(|e| match e {
                jotdown::Event::Start(jotdown::Container::RawBlock { format }, attrs)
                    if format == "html" =>
                {
                    jotdown::Event::Start(jotdown::Container::CodeBlock { language: format }, attrs)
                }
                jotdown::Event::End(jotdown::Container::RawBlock { format })
                    if format == "html" =>
                {
                    jotdown::Event::End(jotdown::Container::CodeBlock { language: format })
                }
                e => e,
            }),
        ));
        Ok(html! {
            article ."m-postOverview"
                #{
                    "post-" (event_id.map(|e| e.to_string()).unwrap_or_else(|| "preview".to_string()))
                } {
                div ."m-postOverview__main" {
                    img ."m-postOverview__userImage u-userImage"
                        src="/assets/icons/circle-user.svg"
                        width="32pt"
                        height="32pt"
                        { }

                    div ."m-postOverview__contentSide" {
                        header .".m-postOverview__header" {
                            span ."m-postOverview__username" { (user_profile.display_name) }
                        }

                        div ."m-postOverview__content" {
                            p {
                                (post_content_rendered)
                            }
                        }
                    }

                }

                @if let Some(ext_event_id) = external_event_id {
                    div ."m-postOverview__buttonBar" {
                        @if reply_count > 0 {
                            button ."m-postOverview__commentsButton u-button"
                                hx-get={"/ui/timeline/comments/"(ext_event_id.event_id().to_short())}
                                hx-target="next .m-postOverview__comments"
                                hx-swap="outerHTML"
                            {
                                span ."m-postOverview__commentsButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                @if reply_count == 1 {
                                    ("1 Comment".to_string())
                                } @else {
                                    (format!("{} Comments", reply_count))
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
                div ."m-postOverview__comments -empty" { }
            }
        })
    }
}
