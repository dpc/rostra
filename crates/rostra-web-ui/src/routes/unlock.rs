use axum::extract::State;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use maud::{html, Markup, PreEscaped};
use rostra_core::id::RostraIdSecretKey;
use serde::Deserialize;

use super::Maud;
use crate::error::RequestResult;
use crate::{SharedState, UiState};

pub async fn get(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.unlock_page("", None).await?))
}

pub async fn get_random(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    Ok(Maud(
        state
            .unlock_page(&RostraIdSecretKey::generate().to_string(), None)
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct Input {
    // Must be password for browsers to offer saving it
    #[serde(rename = "password")]
    mnemonic: String,
}

pub async fn post(state: State<SharedState>, Form(form): Form<Input>) -> RequestResult<Response> {
    Ok(match state.unlock(&form.mnemonic).await {
        Ok(()) => {
            let headers = [(
                HeaderName::from_static("hx-redirect"),
                HeaderValue::from_static("/"),
            )];
            (StatusCode::SEE_OTHER, headers).into_response()
        }
        Err(e) => Maud(
            state
                .unlock_page(
                    &form.mnemonic,
                    html! {
                        span ."unlockScreen_notice" { (e)}
                    },
                )
                .await?,
        )
        .into_response(),
    })
}

impl UiState {
    async fn unlock_page(
        &self,
        current_mnemonic: &str,
        notification: impl Into<Option<Markup>>,
    ) -> RequestResult<Markup> {
        let notification = notification.into();
        let content = html! {
                form ."unlockScreen"
                    autocomplete="on" {
                    @if let Some(n) = notification {
                        (n)
                    }
                    p ."unlockScreen__" { }
                    input ."unlockScreen__fakeUsername"
                        type="username"
                        value="RostraID"
                        required
                        {}
                    input ."unlockScreen__mnemonic"
                        type="password"
                        name="password"
                        autocomplete="current-password"
                        required
                        placeholder="mnemonic"
                        value=(current_mnemonic)
                        { }
                    button ."unlockScreen__unlockButton"
                        type="submit"
                        hx-target="closest .unlockScreen"
                        hx-post="/ui/unlock"
                        { "Unlock" }
                    button
                        ."unlockScreen__generateButton"
                        hx-get="/ui/unlock/random"
                        hx-target="closest .unlockScreen"
                        type="submit"
                        { "Generate" }
                        script {
                            (PreEscaped(
                                r#"
                                ['input', 'change', 'keydown', 'keyup', 'keypress'].forEach(evt => 
                                    document
                                        .querySelector('.unlockScreen__mnemonic')
                                        .dispatchEvent(new Event(evt, {bubbles: true}))
                                );
                            "#))
                        }
                }
        };
        self.html_page("Sign in", content).await
    }
}
