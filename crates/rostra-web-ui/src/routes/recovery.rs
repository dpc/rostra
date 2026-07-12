use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use rostra_core::id::RostraIdSecretKey;

use super::fragment;
use super::unlock::local_redirect::LocalRedirect;

/// Context-specific controls surrounding the shared recovery phrase field.
#[derive(Clone, Copy)]
pub(crate) enum PhrasePanelContext<'a> {
    /// A reveal from the authenticated Settings page.
    Settings,
    /// A newly generated account that has not yet been unlocked.
    AccountCreation {
        /// Safe local destination to visit after account creation.
        redirect: Option<&'a LocalRedirect>,
    },
}

/// Render the recovery phrase controls shared by account creation and Settings.
pub(crate) fn phrase_panel(secret: RostraIdSecretKey, context: PhrasePanelContext<'_>) -> Markup {
    let id = secret.id();
    let phrase = secret.to_string();
    let account_creation = matches!(context, PhrasePanelContext::AccountCreation { .. });
    let redirect = match context {
        PhrasePanelContext::Settings => None,
        PhrasePanelContext::AccountCreation { redirect } => redirect,
    };

    html! {
        div id="recovery-phrase-panel" ."m-recoveryPhrase -revealed"
            data-recovery-phrase
        {
            h3 ."m-recoveryPhrase__title" { "Recovery phrase" }
            p ."m-recoveryPhrase__warning" {
                strong { "Keep this secret." }
                " Anyone with these 24 words can permanently act as you. "
                "Rostra cannot reset or recover them."
            }
            p ."m-recoveryPhrase__guidance" {
                "Save the phrase only in a trusted password manager or offline backup. "
                "Never send it to support or paste it into chat."
            }
            @if account_creation {
                form id="create-account-form" action="/unlock" method="post" {
                    input type="hidden" name="username" value=(id) {}
                    @if let Some(redirect) = redirect {
                        input type="hidden" name="redirect" value=(redirect) {}
                    }
                    (phrase_field(&phrase, Some("password")))
                    label ."m-recoveryPhrase__acknowledgement" {
                        input type="checkbox" data-recovery-ack {}
                        "I saved this recovery phrase in a safe place."
                    }
                    div ."m-recoveryPhrase__actions" {
                        (fragment::button(
                            "m-recoveryPhrase__copyButton",
                            "Copy recovery phrase",
                        )
                        .button_type("button")
                        .onclick("copyRecoveryPhrase(this)")
                        .call())
                        (fragment::button(
                            "m-recoveryPhrase__continueButton",
                            "Continue with new account",
                        )
                        .disabled(true)
                        .call())
                        (fragment::button("m-recoveryPhrase__hideButton", "Hide")
                            .button_type("button")
                            .onclick("hideRecoveryPhrase(this)")
                            .call())
                    }
                }
            } @else {
                (phrase_field(&phrase, None))
                div ."m-recoveryPhrase__actions" {
                    (fragment::button(
                        "m-recoveryPhrase__copyButton",
                        "Copy recovery phrase",
                    )
                    .button_type("button")
                    .onclick("copyRecoveryPhrase(this)")
                    .call())
                    (fragment::button("m-recoveryPhrase__hideButton", "Hide")
                        .button_type("button")
                        .onclick("hideRecoveryPhrase(this)")
                        .call())
                }
            }
            p ."m-recoveryPhrase__status" role="status" aria-live="polite" {}
            script {
                "initializeRecoveryPhrase(document.getElementById('recovery-phrase-panel'));"
            }
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

fn phrase_field(phrase: &str, name: Option<&str>) -> Markup {
    html! {
        label ."m-recoveryPhrase__label" for="recovery-phrase" {
            "24-word recovery phrase"
        }
        textarea id="recovery-phrase" ."m-recoveryPhrase__phrase"
            name=[name]
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
