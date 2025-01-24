use maud::{html, Markup, PreEscaped};

use crate::AppState;

impl AppState {
    pub fn self_account(&self) -> Markup {
        html! {
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
                    src="https://avatars.githubusercontent.com/u/9209?v=4"
                    width="32pt"
                    height="32pt"
                    { }

                div ."m-selfAccount__content" {
                    span ."m-selfAccount__displayName" { "Display Name" }
                    button
                        ."m-selfAccount__copyButton"
                        data-value=(self.id) onclick="copyIdToClipboard(event)" { "ID" }
                }
            }
        }
    }
}
