use axum::extract::State;
use axum::response::IntoResponse;
use axum::Form;
use maud::{html, Markup};
use serde::Deserialize;

use super::super::error::RequestResult;
use super::super::SharedAppState;
use super::Maud;
use crate::fragment::post_overview;
use crate::AppState;

impl AppState {
    pub async fn main_bar_timeline(&self) -> RequestResult<Markup> {
        let posts = self
            .client
            .client_ref()?
            .paginate_social_posts_rev(None, 100)
            .await?;
        Ok(html! {
            @for post in posts {
                div ."o-mainBarTimeline" {
                    div ."o-mainBarTimeline__item" {
                        (post_overview("dpc", "Cool stuff"))
                    }
                }
            }
        })
    }
}
