use maud::{html, Markup};

use super::super::error::RequestResult;
use crate::fragment::post_overview;
use crate::AppState;

impl AppState {
    pub async fn main_bar_timeline(&self) -> RequestResult<Markup> {
        let posts = self
            .client
            .storage()??
            .paginate_social_posts_rev(None, 100)
            .await;
        Ok(html! {
            div ."o-mainBarTimeline" {
                @for post in posts {
                        div ."o-mainBarTimeline__item" {
                            (post_overview(&post.event.author.to_string(), &post.content.djot_content))
                        }
                }
            }
        })
    }
}
