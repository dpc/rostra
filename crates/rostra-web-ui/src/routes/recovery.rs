use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use rostra_core::id::RostraIdSecretKey;

use super::fragment;

/// Render a masked, read-only recovery phrase with a copy control.
pub(crate) fn settings_phrase(secret: RostraIdSecretKey) -> Markup {
    let phrase = secret.to_string();

    html! {
        div ."m-recoveryPhrase__settingsControl" {
            label ."m-recoveryPhrase__label" for="recovery-phrase" {
                "24-word recovery phrase"
            }
            div ."m-recoveryPhrase__settingsControlRow" {
                input id="recovery-phrase" ."m-recoveryPhrase__phrase"
                    type="password"
                    value=(phrase)
                    readonly
                    spellcheck="false"
                    autocapitalize="none"
                    autocorrect="off"
                    autocomplete="off"
                {}
                (copy_button())
            }
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

fn copy_button() -> Markup {
    fragment::button("m-recoveryPhrase__copyButton", "Copy")
        .button_type("button")
        .onclick("copyRecoveryPhrase(this)")
        .aria_label("Copy recovery phrase")
        .requires_js(true)
        .call()
}

#[cfg(test)]
mod tests;
