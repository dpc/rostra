use axum::Form;
use axum::extract::State;
use axum::response::IntoResponse;
use maud::{Markup, html};
use rostra_core::event::PersonasTagsSelector;
use rostra_core::id::RostraId;
use serde::Deserialize;

use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::error::{ReadOnlyModeSnafu, RequestResult};
use crate::{SharedState, UiState};

#[derive(Deserialize)]
pub struct Input {
    rostra_id: RostraId,
}

pub async fn add_followee(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<Input>,
) -> RequestResult<impl IntoResponse> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;
    state
        .client(session.id())
        .await?
        .client_ref()?
        .follow(id_secret, form.rostra_id, PersonasTagsSelector::default())
        .await?;
    Ok(Maud(state.render_add_followee_form(html! {
        span { "Followed!" }
    })))
}

impl UiState {
    pub fn render_add_followee_form(&self, notification: impl Into<Option<Markup>>) -> Markup {
        let notification = notification.into();
        let ajax_attrs = fragment::AjaxLoadingAttrs::for_button();
        html! {
            form id="add-followee-form" ."m-addFolloweeForm"
                action="/followee"
                method="post"
                x-target="add-followee-form"
                "@ajax:before"=(ajax_attrs.before)
                "@ajax:after"=(ajax_attrs.after)
            {
                input ."m-addFolloweeForm__content"
                    placeholder="RostraId"
                    type="text"
                    name="rostra_id"
                    autocomplete="off"
                    {}

                div ."m-addFolloweeForm__bottomBar"{
                    div ."m-addFolloweeForm__notification"{
                        @if let Some(n) = notification {
                                (n)
                        }
                    }
                    (fragment::button("m-addFolloweeForm__followButton", "Follow").call())
                }
            }
        }
    }
}
