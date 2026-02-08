//! Welcome/landing page for unauthenticated users.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use maud::{DOCTYPE, Markup, html};

use super::Maud;
use super::unlock::session::OptionalUserSession;
use crate::SharedState;
use crate::error::RequestResult;

/// Landing page at "/".
/// - Authenticated users: redirect to /home
/// - Unauthenticated + welcome_redirect configured: redirect to that URL
/// - Unauthenticated: show welcome page
pub async fn get_landing(
    state: State<SharedState>,
    session: OptionalUserSession,
) -> RequestResult<impl IntoResponse> {
    // If authenticated, go to home
    if session.0.is_some() {
        return Ok(Redirect::temporary("/home").into_response());
    }

    // If welcome_redirect is configured, use it
    if let Some(ref url) = state.welcome_redirect {
        return Ok(Redirect::temporary(url).into_response());
    }

    // Show welcome page
    let has_default_profile = state.default_profile.is_some();
    Ok(Maud(render_welcome_page(has_default_profile)).into_response())
}

/// Home page - redirects to /followees (may change in future).
pub async fn get_home() -> impl IntoResponse {
    Redirect::temporary("/followees")
}

fn render_welcome_page(has_default_profile: bool) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Rostra - P2P Social Network" }
                link rel="stylesheet" href="/assets/style.css";
            }
            body {
                div ."o-welcomePage" {
                    div ."o-welcomePage__content" {
                        h1 ."o-welcomePage__title" { "Rostra" }
                        p ."o-welcomePage__tagline" {
                            "A censorship-resistant, peer-to-peer social network"
                        }

                        div ."o-welcomePage__features" {
                            div ."o-welcomePage__feature" {
                                h3 { "Sovereign" }
                                p { "No centralized accounts, no censorship. Generate your own cryptographic identity that you fully control." }
                            }
                            div ."o-welcomePage__feature" {
                                h3 { "Authentic" }
                                p { "P2P but without abuse. Content is discovered through people you trust. Spam and attention farming are near impossible." }
                            }
                            div ."o-welcomePage__feature" {
                                h3 { "Private" }
                                p { "Relay-only mode by default keeps your IP hidden. Optional direct connections for faster sync." }
                            }
                            div ."o-welcomePage__feature" {
                                h3 { "Multi-Device" }
                                p { "Use same account on multiple devices. Your content synchronizes automatically between your devices and your followers." }
                            }
                            div ."o-welcomePage__feature" {
                                h3 { "Wholesome" }
                                p { "Post using different personas and follow the parts of people's lives you care about. Share what you want, with whom you want." }
                            }
                            div ."o-welcomePage__feature" {
                                h3 { "Media-Rich" }
                                p { "Share images and videos P2P. No external hosting required." }
                            }
                        }

                        div ."o-welcomePage__actions" {
                            @if has_default_profile {
                                a ."o-welcomePage__button" ."-primary" href="/home" { "Explore" }
                            } @else {
                                a ."o-welcomePage__button" ."-primary" href="/home" { "Sign in" }
                            }
                        }

                        p ."o-welcomePage__footer" {
                            "Open source \u{2022} "
                            a href="https://github.com/dpc/rostra" { "GitHub" }
                        }
                    }
                }
            }
        }
    }
}
