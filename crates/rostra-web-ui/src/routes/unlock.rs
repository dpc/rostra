pub mod session;

use axum::Form;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, html};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use serde::Deserialize;
use session::{OptionalUserSession, SESSION_KEY, UserSessionData};
use snafu::ResultExt as _;
use tower_sessions::Session;

use super::Maud;
use crate::error::{OtherSnafu, PublicKeyMissingSnafu, RequestResult, UnlockResult, UnlockSnafu};
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
    // If we're called due to an AJAX request, that probably means something failed
    // or required auth, and was redirected here. We don't want to respond with
    // a page that Alpine-ajax will interpret as a partial. We want it to reload
    // the page altogether. Use a standard HTTP redirect which Alpine-ajax will
    // follow with a full page reload.
    if is_ajax {
        let url = match &query.redirect {
            Some(path) => format!("/unlock?redirect={}", urlencoding::encode(path)),
            None => "/unlock".to_string(),
        };
        return Ok(Redirect::to(&url).into_response());
    }

    // Pre-populate the RostraId field if we have existing session data
    let existing_id = existing_session.map(|s| s.id());

    Ok(Maud(
        state
            .unlock_page(existing_id, None, None, query.redirect)
            .await?,
    )
    .into_response())
}

pub async fn get_random(
    state: State<SharedState>,
    Query(query): Query<RedirectQuery>,
) -> RequestResult<impl IntoResponse> {
    let random_secret_key = RostraIdSecretKey::generate();
    Ok(Maud(
        state
            .unlock_page(
                Some(random_secret_key.id()),
                Some(random_secret_key),
                None,
                query.redirect,
            )
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
    let redirect_path = form.redirect.clone();
    let rostra_id = form.rostra_id().context(UnlockSnafu)?;

    // 1. Load the client and unlock if secret provided
    let secret_key_opt = match state.unlock(rostra_id, form.mnemonic).await {
        Ok(secret) => secret,
        Err(e) => {
            return Ok(Maud(
                state
                    .unlock_page(
                        form.rostra_id,
                        form.mnemonic,
                        html! {
                            span ."o-unlockScreen_notice" { (e)}
                        },
                        redirect_path,
                    )
                    .await?,
            )
            .into_response());
        }
    };

    // 2. Insert session data into store (this creates/updates the session)
    session
        .insert(SESSION_KEY, &UserSessionData::new(rostra_id))
        .await
        .boxed()
        .context(OtherSnafu)?;

    // 3. Get session ID (now available after insert) and store secret
    // The session ID is available after the session has been modified.
    if let Some(session_token) = SessionToken::from_session(&session) {
        state.set_session_secret(session_token, secret_key_opt);
    }

    // 4. Redirect to the original path if provided, otherwise to root
    let target = redirect_path
        .filter(|p| p.starts_with('/'))
        .unwrap_or_else(|| "/".to_string());
    Ok(Redirect::to(&target).into_response())
}

pub async fn logout(session: Session) -> RequestResult<impl IntoResponse> {
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
        redirect: Option<String>,
    ) -> RequestResult<Markup> {
        let random_rostra_id_secret = &RostraIdSecretKey::generate();
        let random_mnemonic = random_rostra_id_secret.to_string();
        let random_rostra_id = random_rostra_id_secret.id().to_string();
        let notification = notification.into();
        let content = html! {
            div id="unlock-screen" ."o-unlockScreen" {

                form ."o-unlockScreen__form"
                    action="/unlock"
                    method="post"
                    autocomplete="on"
                {
                    @if let Some(ref redirect_path) = redirect {
                        input type="hidden" name="redirect" value=(redirect_path) {}
                    }
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
                        {
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
        self.render_html_page("Sign in", content, None).await
    }
}
