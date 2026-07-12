pub(crate) mod local_redirect;
pub mod session;

use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use local_redirect::LocalRedirect;
use maud::{Markup, html};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util_error::FmtCompact as _;
use serde::Deserialize;
use session::{OptionalUserSession, SESSION_KEY, UserSessionData};
use snafu::ResultExt as _;
use tower_sessions::Session;

use super::{Maud, fragment, recovery};
use crate::error::{
    OtherSnafu, PublicKeyMissingSnafu, RequestResult, UnlockResult, UnlockSnafu, UserErrorResponse,
};
use crate::serde_util::empty_string_as_none;
use crate::util::extractors::AjaxRequest;
use crate::{SessionToken, SharedState, UiState};

#[derive(Deserialize)]
pub struct RedirectQuery {
    redirect: Option<String>,
}

pub async fn get(
    state: State<SharedState>,
    OptionalUserSession(existing_session): OptionalUserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Query(query): Query<RedirectQuery>,
) -> RequestResult<impl IntoResponse> {
    // AJAX requests arrive here after fetch() auto-follows a 303 redirect
    // from an auth-required route. Returning another 303 would cause an
    // infinite loop (the X-Alpine-Request header is preserved across
    // redirects). Instead, return 401 JSON so the JS error handler can
    // show a toast and navigate to the login page.
    if is_ajax {
        return Ok((
            StatusCode::UNAUTHORIZED,
            super::AppJson(UserErrorResponse {
                message: "Session expired. Please log in again.".to_string(),
            }),
        )
            .into_response());
    }

    // Pre-populate the RostraId field if we have existing session data
    let existing_id = existing_session.map(|s| s.id());

    let redirect = query.redirect.as_deref().and_then(LocalRedirect::parse);
    Ok(Maud(
        state
            .unlock_page(existing_id, None, None, redirect.as_ref())
            .await?,
    )
    .into_response())
}

/// Generate an account credential and return its protected backup controls.
pub async fn post_generate(state: State<SharedState>, Form(form): Form<RedirectQuery>) -> Response {
    if !state.recovery_transport_secure() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let redirect = form.redirect.as_deref().and_then(LocalRedirect::parse);
    recovery::sensitive_response(Maud(recovery::phrase_panel(
        rostra_core::id::RostraIdSecretKey::generate(),
        recovery::PhrasePanelContext::AccountCreation {
            redirect: redirect.as_ref(),
        },
    )))
}

#[derive(Deserialize)]
pub struct Input {
    #[serde(rename = "username")]
    #[serde(deserialize_with = "empty_string_as_none")]
    rostra_id: Option<RostraId>,
    #[serde(rename = "password")]
    #[serde(deserialize_with = "empty_string_as_none")]
    mnemonic: Option<RostraIdSecretKey>,
    #[serde(default)]
    #[serde(deserialize_with = "empty_string_as_none")]
    redirect: Option<String>,
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
    let redirect_path = form.redirect.as_deref().and_then(LocalRedirect::parse);
    let rostra_id = form.rostra_id().context(UnlockSnafu)?;

    // 1. Load the client and unlock if secret provided
    let secret_key_opt = match state.unlock(rostra_id, form.mnemonic).await {
        Ok(secret) => secret,
        Err(e) => {
            let error_message = e.fmt_compact().to_string();
            return Ok(recovery::sensitive_response(Maud(
                state
                    .unlock_page(
                        form.rostra_id,
                        form.mnemonic,
                        html! {
                            span ."o-unlockScreen_notice" { (error_message)}
                        },
                        redirect_path.as_ref(),
                    )
                    .await?,
            )));
        }
    };

    // 2. Insert session data and save to store
    session
        .insert(SESSION_KEY, &UserSessionData::new(rostra_id))
        .await
        .boxed()
        .context(OtherSnafu)?;
    session.save().await.boxed().context(OtherSnafu)?;

    // 3. Get session ID (now available after save) and store secret
    if let Some(session_token) = SessionToken::from_session(&session) {
        state.set_session_secret(session_token, secret_key_opt);

        // 4. Opportunistically clean up secrets for expired sessions (neighbors only)
        state.gc_secrets(session_token).await;
    }

    // 5. Redirect to the original path if provided, otherwise to root
    let target = redirect_path.as_ref().map_or("/", LocalRedirect::as_str);
    Ok(Redirect::to(target).into_response())
}

pub async fn logout(
    state: State<SharedState>,
    session: Session,
) -> RequestResult<impl IntoResponse> {
    if let Some(token) = SessionToken::from_session(&session) {
        state.set_session_secret(token, None);
    }
    session.delete().await.boxed().context(OtherSnafu)?;

    // Use standard HTTP redirect for Alpine-ajax
    Ok(Redirect::to("/unlock").into_response())
}

impl UiState {
    async fn unlock_page(
        &self,
        current_rostra_id: Option<RostraId>,
        current_secret_key: Option<RostraIdSecretKey>,
        notification: impl Into<Option<Markup>>,
        redirect: Option<&LocalRedirect>,
    ) -> RequestResult<Markup> {
        let notification = notification.into();
        let content = html! {
            div id="unlock-screen" ."o-unlockScreen" {

                form ."o-unlockScreen__form"
                    action="/unlock"
                    method="post"
                    autocomplete="on"
                {
                    @if let Some(redirect_path) = redirect {
                        input type="hidden" name="redirect" value=(redirect_path) {}
                    }
                    @if let Some(n) = notification {
                        (n)
                    }
                    div ."o-unlockScreen__header"  {
                        h4 { "Login to Rostra" }
                        p { "Provide existing RostraID (public key) only to log in in read-only mode," }
                        p { "or Rostra secret passphrase to log in normally." }
                        p {
                            "Create Account to generate a new Rostra identity. Save its recovery phrase in your password manager."
                        }
                    }
                    div."o-unlockScreen__idLine" {
                        input ."o-unlockScreen__id"
                            type="username"
                            name="username"
                            placeholder="Id (Public Key)"
                            autocomplete="username"
                            title="Id is a single, long string of characters encoding your public identifier"
                            value=(current_rostra_id.map(|id| id.to_string()).unwrap_or_default())
                            autofocus
                            {}
                        (fragment::button("o-unlockScreen__unlockButton", "Login").call())
                    }
                    div."o-unlockScreen__mnemonicLine" {
                        input ."o-unlockScreen__mnemonic"
                            type="password"
                            name="password"
                            autocomplete="current-password"
                            placeholder="Mnemonic (Secret Key) - if empty, read-only mode"
                            title="The mnemonic is a 24-word phrase encoding your identity's secret key"
                            value=(current_secret_key.as_ref().map(ToString::to_string).unwrap_or_default())
                            { }
                        (fragment::button("o-unlockScreen__roButton", "Clear\u{a0}secret")
                            .button_type("button")
                            .onclick("document.querySelector('.o-unlockScreen__mnemonic').value = '';")
                            .title("Clear the mnemonic to login in read-only mode")
                            .call())
                    }
                }
                form ."o-unlockScreen__unlockLine"
                    action="/unlock/generate"
                    method="post"
                    x-target="account-recovery-target"
                {
                    @if let Some(redirect_path) = redirect {
                        input type="hidden" name="redirect" value=(redirect_path) {}
                    }
                    (fragment::button("o-unlockScreen__generateButton", "Create Account")
                        .disabled(!self.recovery_transport_secure())
                        .title("Generate a new account and show its 24-word recovery phrase.")
                        .call())
                }
                @if !self.recovery_transport_secure() {
                    p ."o-unlockScreen_notice" {
                        "Account creation is disabled because this server is not configured for HTTPS or loopback-only access."
                    }
                }
                div id="account-recovery-target" {}
            }
        };
        self.render_html_page("Sign in", content, None, None, None, false)
            .await
    }
}
