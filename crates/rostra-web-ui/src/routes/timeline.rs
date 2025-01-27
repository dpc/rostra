use axum::extract::State;
use axum::response::IntoResponse;
use maud::{html, Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::id::RostraId;

use super::super::error::RequestResult;
use super::Maud;
use crate::{SharedState, UiState};

pub async fn get(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.timeline_page().await?))
}

impl UiState {
    pub async fn timeline_page(&self) -> RequestResult<Markup> {
        let content = html! {
            nav ."o-navBar" {

                div ."o-navBar__selfAccount" {
                    (self.render_self_profile_summary().await?)
                }

                (self.new_post_form(None))

                (self.add_followee_form(None))

                div ."o-navBar__list" {
                    span ."o-navBar_header" { "Rostra:" }
                    a ."o-navBar__item" href="https://github.com/dpc/rostra" { "Github" }
                    // a ."o-navBar__item" href="/" { "Something" }
                }
            }

            main ."o-mainBar" {
                (self.main_bar_timeline().await?)
            }

        };

        self.html_page("You're Rostra!", content).await
    }

    pub async fn main_bar_timeline(&self) -> RequestResult<Markup> {
        let client = self.client().await?;
        let client_ref = client.client_ref()?;

        let posts = self
            .client()
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
                        src="https://avatars.githubusercontent.com/u/9209?v=4"
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
