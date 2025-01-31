pub mod session;

use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use maud::{html, Markup};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use serde::Deserialize;
use session::{UserSession, SESSION_KEY};
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
                    &UserSession::new(form.rostra_id, secret_key_opt),
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
                        span ."o-unlockScreen_notice" { (e)}
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
            div ."o-unlockScreen" {

                form ."o-unlockScreen__form"
                    autocomplete="on" {
                    @if let Some(n) = notification {
                        (n)
                    }
                    div ."o-unlockScreen__header"  {
                        h4 { "Unlock Rostra account" }
                        p { "Provide existing RostraId, secret passphrase or both to unlock your identity."}
                        p { "Use Random button to generate new identity."}
                        p { "Make sure to save your identity information."}
                    }
                    div."o-unlockScreen__idLine" {
                        input ."o-unlockScreen__id"
                            type="username"
                            name="username"
                            placeholder="Id..."
                            title="Id is a single, long string of characters encoding your public identifier"
                            value=(current_rostra_id.map(|id| id.to_string()).unwrap_or_default())
                            {}
                        button ."o-unlockScreen__unlockButton u-button"
                            type="submit"
                            hx-target="closest .o-unlockScreen"
                            hx-post="/ui/unlock" {
                                span ."o-unlockScreen__unlockButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Unlock"
                            }
                    }
                    div."o-unlockScreen__mnemonicLine" {
                        input ."o-unlockScreen__mnemonic"
                            type="password"
                            name="password"
                            autocomplete="current-password"
                            placeholder="Mnemonic... (optional in read-only mode)"
                            title="Mnemonic is 12 words passphrase encoding secret key of your identity"
                            value=(current_mnemonic)
                            { }
                        button
                            type="button" // do not submit the form!
                            ."o-unlockScreen__roButton u-button"
                            onclick=(
                                r#"
                                    document.querySelector('.o-unlockScreen__mnemonic').value = '';
                                "#
                            )
                            title="Clear the mnemonic to unlock in a read-only mode"
                            {
                                span ."o-unlockScreen__roButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Read-only"
                            }
                    }
                    div."o-unlockScreen__unlockLine" {
                        button
                            type="button" // do not submit the form!
                            ."o-unlockScreen__generateButton u-button"
                            onclick=(
                                format!(r#"
                                    document.querySelector('.o-unlockScreen__rostraId').value = '{}';
                                    document.querySelector('.o-unlockScreen__mnemonic').value = '{}';
                                "#, random_rostra_id, random_mnemonic)
                            )
                            title="Generate a random account."
                            {
                                span ."o-unlockScreen__generateButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Random"
                            }
                    }
                }
            }
        };
        self.render_html_page("Sign in", content).await
    }
}
