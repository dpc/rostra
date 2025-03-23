pub mod session;

use axum::Form;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use serde::Deserialize;
use session::{SESSION_KEY, UserSession};
use snafu::ResultExt as _;
use tower_sessions::Session;

use super::Maud;
use crate::error::{
    LoginRequiredSnafu, OtherSnafu, PublicKeyMissingSnafu, RequestResult, UnlockResult, UnlockSnafu,
};
use crate::is_htmx::IsHtmx;
use crate::serde_util::empty_string_as_none;
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
    Ok(Maud(state.unlock_page(None, None, None).await?).into_response())
}

pub async fn get_random(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    let random_secret_key = RostraIdSecretKey::generate();
    Ok(Maud(
        state
            .unlock_page(Some(random_secret_key.id()), Some(random_secret_key), None)
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct Input {
    #[serde(rename = "username")]
    #[serde(deserialize_with = "empty_string_as_none")]
    rostra_id: Option<RostraId>,
    #[serde(rename = "password")]
    #[serde(deserialize_with = "empty_string_as_none")]
    mnemonic: Option<RostraIdSecretKey>,
}

impl Input {
    fn rostra_id(&self) -> UnlockResult<RostraId> {
        self.rostra_id
            .or_else(|| self.mnemonic.map(|m| m.id()))
            .ok_or_else(|| PublicKeyMissingSnafu.build())
    }
}

pub async fn post_unlock(
    state: State<SharedState>,
    session: Session,
    Form(form): Form<Input>,
) -> RequestResult<Response> {
    Ok(
        match state
            .unlock(form.rostra_id().context(UnlockSnafu)?, form.mnemonic)
            .await
        {
            Ok(secret_key_opt) => {
                let rostra_id = secret_key_opt
                    .map(|secret| secret.id())
                    .or(form.rostra_id)
                    .ok_or_else(|| LoginRequiredSnafu.build())?;
                session
                    .insert(SESSION_KEY, &UserSession::new(rostra_id, secret_key_opt))
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
                        form.rostra_id,
                        form.mnemonic,
                        html! {
                            span ."o-unlockScreen_notice" { (e)}
                        },
                    )
                    .await?,
            )
            .into_response(),
        },
    )
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
        current_secret_key: Option<RostraIdSecretKey>,
        notification: impl Into<Option<Markup>>,
    ) -> RequestResult<Markup> {
        let random_rostra_id_secret = &RostraIdSecretKey::generate();
        let random_mnemonic = random_rostra_id_secret.to_string();
        let random_rostra_id = random_rostra_id_secret.id().to_string();
        let notification = notification.into();
        let content = html! {
            div ."o-unlockScreen" {

                form ."o-unlockScreen__form"
                    method="post"
                    autocomplete="on"
                {
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
                            placeholder="Id (Public Key)"
                            autocomplete="username"
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
                            placeholder="Mnemonic (Secret Key) - if empty, read-only mode"
                            title="Mnemonic is 12 words passphrase encoding secret key of your identity"
                            value=(current_secret_key.as_ref().map(ToString::to_string).unwrap_or_default())
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
                                    document.querySelector('.o-unlockScreen__id').value = '{}';
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
