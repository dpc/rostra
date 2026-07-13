use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use rostra_core::id::RostraIdSecretKey;

use super::fragment;
use super::unlock::local_redirect::LocalRedirect;

/// Semantic presentation of a recovery phrase field.
#[derive(Clone, Copy)]
enum PhraseFieldMode {
    /// Selectable credential submitted during account creation.
    AccountCreation,
    /// Masked credential displayed in authenticated Settings.
    MaskedSettings,
}

/// Render the generated credential and account creation form.
pub(crate) fn account_creation_panel(
    secret: RostraIdSecretKey,
    redirect: Option<&LocalRedirect>,
) -> Markup {
    let id = secret.id();
    let phrase = secret.to_string();

    html! {
        div ."m-recoveryPhrase" {
            h2 ."m-recoveryPhrase__title" { "Recovery phrase" }
            p ."m-recoveryPhrase__warning" {
                strong { "Keep this secret." }
                " Anyone with these 24 words can permanently act as you. "
                "Rostra cannot reset or recover them."
            }
            p ."m-recoveryPhrase__guidance" {
                "Save the phrase only in a trusted password manager or offline backup. "
                "Never send it to support or paste it into chat."
            }
            form id="create-account-form" action="/unlock" method="post" {
                input type="hidden" name="username" value=(id) {}
                @if let Some(redirect) = redirect {
                    input type="hidden" name="redirect" value=(redirect) {}
                }
                (phrase_field(&phrase, PhraseFieldMode::AccountCreation))
                div ."m-recoveryPhrase__actions" {
                    (copy_button())
                    (fragment::button(
                        "m-recoveryPhrase__continueButton",
                        "Continue with new account",
                    )
                        .call())
                }
            }
            p ."m-recoveryPhrase__status" role="status" aria-live="polite" {}
        }
    }
}

/// Render a masked, read-only recovery phrase with a copy control.
pub(crate) fn settings_phrase(secret: RostraIdSecretKey) -> Markup {
    let phrase = secret.to_string();

    html! {
        div ."m-recoveryPhrase__settingsControl" {
            (phrase_field(&phrase, PhraseFieldMode::MaskedSettings))
            (copy_button())
            p ."m-recoveryPhrase__status" role="status" aria-live="polite" {}
        }
    }
}

/// Add response controls required whenever recovery credentials may be present.
pub(crate) fn sensitive_response(body: impl IntoResponse) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response.headers_mut().insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    response
}

fn phrase_field(phrase: &str, mode: PhraseFieldMode) -> Markup {
    html! {
        label ."m-recoveryPhrase__label" for="recovery-phrase" {
            "24-word recovery phrase"
        }
        @if matches!(mode, PhraseFieldMode::MaskedSettings) {
            input id="recovery-phrase" ."m-recoveryPhrase__phrase"
                type="password"
                value=(phrase)
                readonly
                spellcheck="false"
                autocapitalize="none"
                autocorrect="off"
                autocomplete="off"
            {}
        } @else {
            textarea id="recovery-phrase" ."m-recoveryPhrase__phrase"
                name="password"
                readonly
                rows="5"
                spellcheck="false"
                autocapitalize="none"
                autocorrect="off"
                autocomplete="off"
            {
                (phrase)
            }
        }
    }
}

fn copy_button() -> Markup {
    fragment::button("m-recoveryPhrase__copyButton", "Copy")
        .button_type("button")
        .onclick("copyRecoveryPhrase(this)")
        .aria_label("Copy recovery phrase")
        .requires_js(true)
        .call()
}
