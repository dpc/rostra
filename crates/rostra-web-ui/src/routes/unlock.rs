pub mod session;

use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use maud::{html, Markup};
use rostra_core::id::{RostraId, RostraIdSecretKey};
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
    Ok(Maud(state.unlock_page(None, "", None).await?).into_response())
}

pub async fn get_random(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    let random_secret_key = RostraIdSecretKey::generate();
    Ok(Maud(
        state
            .unlock_page(
                Some(random_secret_key.id()),
                &random_secret_key.to_string(),
                None,
            )
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct Input {
    #[serde(rename = "username")]
    rostra_id: RostraId,
    #[serde(rename = "password")]
    mnemonic: String,
}

pub async fn post_unlock(
    state: State<SharedState>,
    session: Session,
    Form(form): Form<Input>,
) -> RequestResult<Response> {
    Ok(match state.unlock(form.rostra_id, &form.mnemonic).await {
        Ok(secret_key_opt) => {
            session
                .insert(
                    SESSION_KEY,
                    &AuthenticatedUser::new(form.rostra_id, secret_key_opt),
                )
                .await
                .boxed()
                .context(OtherSnafu)?;
            let headers = [(
                HeaderName::from_static("hx-redirect"),
                HeaderValue::from_static("/ui"),
            )];
            (StatusCode::SEE_OTHER, headers).into_response()
        }
        Err(e) => Maud(
            state
                .unlock_page(
                    Some(form.rostra_id),
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
        HeaderValue::from_static("/ui"),
    )];
    Ok((StatusCode::SEE_OTHER, headers).into_response())
}

impl UiState {
    async fn unlock_page(
        &self,
        current_rostra_id: Option<RostraId>,
        current_mnemonic: &str,
        notification: impl Into<Option<Markup>>,
    ) -> RequestResult<Markup> {
        let random_rostra_id_secret = &RostraIdSecretKey::generate();
        let random_mnemonic = random_rostra_id_secret.to_string();
        let random_rostra_id = random_rostra_id_secret.id().to_string();
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
                    input ."unlockScreen__rostraId"
                        type="username"
                        name="username"
                        placeholder="RostraId"
                        value=(current_rostra_id.map(|id| id.to_string()).unwrap_or_default())
                        {}
                    div."unlockScreen__unlockLine" {
                        input ."unlockScreen__mnemonic"
                            type="password"
                            name="password"
                            autocomplete="current-password"
                            placeholder="mnemonic (optional in read-only mode)"
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
                        onclick=(
                            format!(r#"
                                document.querySelector('.unlockScreen__rostraId').value = '{}';
                                document.querySelector('.unlockScreen__mnemonic').value = '{}';
                            "#, random_rostra_id, random_mnemonic)
                        )
                        { "Generate" }
                }
            }
        };
        self.render_html_page("Sign in", content).await
    }
}
