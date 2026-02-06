use rostra_client_db::social::ReceivedAtPaginationCursor;
use rostra_core::id::ShortRostraId;
use rostra_util_error::FmtCompact as _;
use tower_cookies::{Cookie, Cookies};
use tracing::debug;

use crate::LOG_TARGET;

const NOTIFICATIONS_LAST_SEEN_COOKIE_NAME: &str = "notifications-last-seen";
const PERSONA_COOKIE_NAME: &str = "persona";

pub(crate) trait CookiesExt {
    /// Get the last seen notification cursor for received-time ordering.
    fn get_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor>;

    /// Save the last seen notification cursor for received-time ordering.
    fn save_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    );

    fn get_persona(&self, self_id: impl Into<ShortRostraId>) -> Option<u8>;

    fn save_persona(&mut self, self_id: impl Into<ShortRostraId>, persona_id: u8);
}

impl CookiesExt for Cookies {
    fn get_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor> {
        let self_id = self_id.into();
        if let Some(s) = self.get(&format!("{self_id}-{NOTIFICATIONS_LAST_SEEN_COOKIE_NAME}")) {
            serde_json::from_str(s.value())
                .inspect_err(|err| {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Invalid cookie value");
                })
                .ok()
        } else {
            None
        }
    }

    fn save_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    ) {
        let self_id = self_id.into();
        let mut cookie = Cookie::new(
            format!("{self_id}-{NOTIFICATIONS_LAST_SEEN_COOKIE_NAME}"),
            serde_json::to_string(&pagination).expect("can't fail"),
        );
        cookie.set_path("/ui");
        cookie.set_max_age(time::Duration::weeks(50));
        self.add(cookie);
    }

    fn get_persona(&self, self_id: impl Into<ShortRostraId>) -> Option<u8> {
        let self_id = self_id.into();
        if let Some(s) = self.get(&format!("{self_id}-{PERSONA_COOKIE_NAME}")) {
            s.value().parse::<u8>()
                .inspect_err(|err| {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Invalid persona cookie value");
                })
                .ok()
        } else {
            None
        }
    }

    fn save_persona(&mut self, self_id: impl Into<ShortRostraId>, persona_id: u8) {
        let self_id = self_id.into();
        let mut cookie = Cookie::new(
            format!("{self_id}-{PERSONA_COOKIE_NAME}"),
            persona_id.to_string(),
        );
        cookie.set_path("/ui");
        cookie.set_max_age(time::Duration::weeks(50));
        self.add(cookie);
    }
}
