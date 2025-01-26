use axum::extract::State;
use axum::response::IntoResponse;
use maud::{html, Markup};

use super::super::error::RequestResult;
use super::Maud;
use crate::fragment::post_overview;
use crate::{SharedState, UiState};

pub async fn get(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.timeline_page().await?))
}

impl UiState {
    pub async fn timeline_page(&self) -> RequestResult<Markup> {
        let content = html! {
            nav ."o-navBar" {

                div ."o-navBar__selfAccount" {
                    (self.self_account().await?)
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
        let posts = self
            .client()
            .await?
            .storage()??
            .paginate_social_posts_rev(None, 100)
            .await;
        Ok(html! {
            div ."o-mainBarTimeline" {
                div ."o-mainBarTimeline__preview" { }
                @for post in posts {
                    div ."o-mainBarTimeline__item" {
                        (post_overview(&post.event.author.to_string(), &post.content.djot_content))
                    }
                }
            }
        })
    }
}
