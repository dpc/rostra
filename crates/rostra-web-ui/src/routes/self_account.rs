use maud::{html, Markup};

use crate::AppState;

impl AppState {
    pub fn self_account(&self) -> Markup {
        html! {
            p { (self.id) }
        }
    }
}
