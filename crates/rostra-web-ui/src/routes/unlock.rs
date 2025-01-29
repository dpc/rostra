pub mod session;

use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use maud::{html, Markup};
use rostra_core::id::RostraIdSecretKey;
use serde::Deserialize;
use session::{AuthenticatedUser, SESSION_KEY};
use snafu::ResultExt as _;
use tower_sessions::Session;

use super::Maud;
use crate::error::{OtherSnafu, RequestResult};
use crate::is_htmx::IsHtmx;
use crate::{SharedState, UiState};

pub async fn get(
    state: State<SharedState>,
    IsHtmx(is_htmx): IsHtmx,
) -> RequestResult<impl IntoResponse> {
    // If we're called due to htmx request, that probably means something failed or
    // required a auth, and was redirected here with HTTP header. We don't want
    // to respond with a page, that htmx will interpret as a partial. We want it
    // to reload the page altogether.
    if is_htmx {
        let headers = [(
            HeaderName::from_static("hx-redirect"),
            HeaderValue::from_static("/ui/unlock"),
        )];
        return Ok((StatusCode::OK, headers).into_response());
    }
    Ok(Maud(state.unlock_page("", None).await?).into_response())
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

pub async fn post(
    state: State<SharedState>,
    session: Session,
    Form(form): Form<Input>,
) -> RequestResult<Response> {
    Ok(match state.unlock(&form.mnemonic).await {
        Ok(secret_key) => {
            session
                .insert(SESSION_KEY, &AuthenticatedUser { secret_key })
                .await
                .boxed()
                .context(OtherSnafu)?;
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

pub async fn logout(session: Session) -> RequestResult<impl IntoResponse> {
    session.delete().await.boxed().context(OtherSnafu)?;

    let headers = [(
        HeaderName::from_static("hx-redirect"),
        HeaderValue::from_static("/"),
    )];
    Ok((StatusCode::SEE_OTHER, headers).into_response())
}

impl UiState {
    async fn unlock_page(
        &self,
        current_mnemonic: &str,
        notification: impl Into<Option<Markup>>,
    ) -> RequestResult<Markup> {
        let random_mnemonic = &RostraIdSecretKey::generate().to_string();
        let notification = notification.into();
        let content = html! {
            div ."unlockScreen" {

                form ."unlockScreen__form"
                    autocomplete="on" {
                    @if let Some(n) = notification {
                        (n)
                    }
                    // some browsers might refuse to save if there's no "username"
                    div ."unlockScreen__header"  {

                        h4 { "Welcome to Rostra!" }
                        p { "To use Rostra you need to paste an existing mnemonic or generate a new one."}
                        p { "Make sure you save it in your browser's password manager and make a backup."}
                    }
                    input ."unlockScreen__fakeUsername"
                        type="username"
                        value="RostraId"
                        required
                        {}
                    div."unlockScreen__unlockLine" {
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
                    }
                    button
                        type="button" // do not submit the form!
                        ."unlockScreen__generateButton"
                        onclick={"document.querySelector('.unlockScreen__mnemonic').value = '" (random_mnemonic) "';"}
                        { "Generate" }
                }
            }
        };
        self.render_html_page("Sign in", content).await
    }
}
