use axum::extract::State;
use axum::response::IntoResponse;
use maud::{html, Markup, PreEscaped};

use super::Maud;
use crate::error::RequestResult;
use crate::{SharedState, UiState};

pub async fn get_self_account_edit(state: State<SharedState>) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.self_account_edit().await?))
}

impl UiState {
    pub async fn self_display_name(&self) -> String {
        "TDB: Display Name".into()
    }

    pub async fn self_avatar_url(&self) -> String {
        "/assets/icons/circle-user.svg".into()
    }

    pub async fn self_account(&self) -> RequestResult<Markup> {
        Ok(html! {
            div ."m-selfAccount" {
                script {
                    (PreEscaped(
                    r#"
                    function copyIdToClipboard(event) {
                        const target = event.target;
                        const id = target.getAttribute('data-value');
                        navigator.clipboard.writeText(id);
                        target.classList.add('-active');

                        setTimeout(() => {
                            target.classList.remove('-active');
                        }, 1000);
                    }
                    "#
                    ))
                }
                img ."m-selfAccount__userImage"
                    src=(self.self_avatar_url().await)
                    width="32pt"
                    height="32pt"
                    { }

                div ."m-selfAccount__content" {
                    span ."m-selfAccount__displayName" { (self.self_display_name().await) }
                    div ."m-selfAccount__buttons" {
                        button
                            ."m-selfAccount__copyButton"
                            data-value=(self.client().await?.client_ref()?.rostra_id()) onclick="copyIdToClipboard(event)"  {
                                span ."m-selfAccount__copyButtonIcon" width="1rem" height="1rem" {}
                                "RostraId"
                            }
                        button
                            ."m-selfAccount__editButton"
                            data-value=(self.client().await?.client_ref()?.rostra_id())
                            hx-get="/ui/self/edit"
                            hx-target="closest .m-selfAccount"
                            hx-swap="outerHTML"
                            {
                                span ."m-selfAccount__editButtonIcon" width="1rem" height="1rem" {}
                                "Edit"
                            }
                    }
                }
            }
        })
    }

    pub async fn self_account_edit(&self) -> RequestResult<Markup> {
        Ok(html! {
            form ."m-selfAccount" {

                img ."m-selfAccount__userImage"
                    src=(self.self_avatar_url().await)
                    width="32pt"
                    height="32pt"
                    { }
                div ."m-selfAccount__content" {
                    input type="text" ."m-selfAccount__displayName" value=(self.self_display_name().await) {  }

                    div ."m-selfAccount__buttons" {
                        button
                            ."m-selfAccount__saveButton"
                            {
                                span ."m-selfAccount__saveButtonIcon" width="1rem" height="1rem" {}
                                "Save"
                            }
                    }
                }
            }
        })
    }
}
