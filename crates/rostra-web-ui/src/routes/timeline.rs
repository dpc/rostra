use std::future::pending;
use std::time::Duration;

use axum::extract::ws::WebSocket;
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use maud::{html, Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::event::EventKind;
use rostra_core::id::RostraId;
use rostra_util_error::FmtCompact as _;
use tracing::debug;

use super::super::error::RequestResult;
use super::unlock::session::AuthenticatedUser;
use super::Maud;
use crate::{SharedState, UiState, LOG_TARGET};

pub async fn get(
    state: State<SharedState>,
    _user: AuthenticatedUser,
    session: AuthenticatedUser,
) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.timeline_page(&session).await?))
}

pub async fn get_updates(
    state: State<SharedState>,
    ws: WebSocketUpgrade,
    session: AuthenticatedUser,
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

impl UiState {
    async fn handle_get_updates(
        &self,
        mut ws: WebSocket,
        user: &AuthenticatedUser,
    ) -> RequestResult<()> {
        let client = self.client(user.id()).await?;
        let self_id = client.client_ref()?.rostra_id();
        let Some(mut new_posts) = client.client_ref()?.new_content_subscribe() else {
            pending::<()>().await;
            return Ok(());
        };

        let mut count = 0;

        loop {
            let event_content = match new_posts.recv().await {
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
                        (self.new_posts_alert(true, count))
                    }
                    .into_string()
                    .into(),
                )
                .await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    pub async fn timeline_page(&self, user: &AuthenticatedUser) -> RequestResult<Markup> {
        let content = html! {
            nav ."o-navBar" hx-ext="ws" ws-connect="/ui/timeline/updates" {

                div ."o-navBar__selfAccount" {
                    (self.render_self_profile_summary(user).await?)
                }

                (self.new_post_form(None))

                (self.render_add_followee_form(None))

                div ."o-navBar__list" {
                    span ."o-navBar_header" { "Rostra:" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra" { "Github" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra/discussions" { "Forum" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra/wiki" { "Wiki" }
                    // a ."o-navBar__item" href="/" { "Something" }
                }
            }

            main ."o-mainBar" {
                (self.new_posts_alert(false, 0))
                (self.main_bar_timeline(user).await?)
            }

        };

        self.render_html_page("You're Rostra!", content).await
    }

    pub fn new_posts_alert(&self, visible: bool, count: u64) -> Markup {
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

    pub async fn main_bar_timeline(&self, user: &AuthenticatedUser) -> RequestResult<Markup> {
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
                        (self.post_overview(&client_ref, post.event.author, &post.content.djot_content).await?)
                    }
                }
            }
        })
    }

    pub async fn post_overview(
        &self,
        client: &ClientRef<'_>,
        author: RostraId,
        content: &str,
    ) -> RequestResult<Markup> {
        let user_profile = self.get_social_profile(author, client).await?;

        let content_html =
            jotdown::html::render_to_string(jotdown::Parser::new(content).map(|e| match e {
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
            }));
        Ok(html! {
            article ."m-postOverview" {
                div ."m-postOverview__main" {
                    img ."m-postOverview__userImage"
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
                                (PreEscaped(content_html))
                            }
                        }
                    }
                }

                div ."m-postOverview__buttonBar"{
                    // "Buttons here"
                }
            }
        })
    }
}
