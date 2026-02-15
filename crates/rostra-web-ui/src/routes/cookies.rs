use rostra_client_db::social::ReceivedAtPaginationCursor;
use rostra_core::id::ShortRostraId;
use rostra_util_error::FmtCompact as _;
use tower_cookies::{Cookie, Cookies};
use tracing::debug;

use crate::LOG_TARGET;

const NOTIFICATIONS_LAST_SEEN_COOKIE_NAME: &str = "notifications-last-seen";
const FOLLOWEES_LAST_SEEN_COOKIE_NAME: &str = "followees-last-seen";
const NETWORK_LAST_SEEN_COOKIE_NAME: &str = "network-last-seen";
const SHOUTBOX_LAST_SEEN_COOKIE_NAME: &str = "shoutbox-last-seen";
const PERSONA_COOKIE_NAME: &str = "persona";

pub(crate) trait CookiesExt {
    fn get_notifications_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor>;

    fn save_notifications_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    );

    fn get_followees_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor>;

    fn save_followees_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    );

    fn get_network_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor>;

    fn save_network_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    );

    fn get_shoutbox_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor>;

    fn save_shoutbox_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    );

    fn get_persona(&self, self_id: impl Into<ShortRostraId>) -> Option<u8>;

    fn save_persona(&mut self, self_id: impl Into<ShortRostraId>, persona_id: u8);
}

fn get_cursor(
    cookies: &Cookies,
    self_id: impl Into<ShortRostraId>,
    cookie_name: &str,
) -> Option<ReceivedAtPaginationCursor> {
    let self_id = self_id.into();
    if let Some(s) = cookies.get(&format!("{self_id}-{cookie_name}")) {
        serde_json::from_str(s.value())
            .inspect_err(|err| {
                debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Invalid cookie value");
            })
            .ok()
    } else {
        None
    }
}

fn save_cursor(
    cookies: &mut Cookies,
    self_id: impl Into<ShortRostraId>,
    cookie_name: &str,
    pagination: ReceivedAtPaginationCursor,
) {
    let self_id = self_id.into();
    let mut cookie = Cookie::new(
        format!("{self_id}-{cookie_name}"),
        serde_json::to_string(&pagination).expect("can't fail"),
    );
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::weeks(50));
    cookies.add(cookie);
}

impl CookiesExt for Cookies {
    fn get_notifications_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor> {
        get_cursor(self, self_id, NOTIFICATIONS_LAST_SEEN_COOKIE_NAME)
    }

    fn save_notifications_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    ) {
        save_cursor(
            self,
            self_id,
            NOTIFICATIONS_LAST_SEEN_COOKIE_NAME,
            pagination,
        );
    }

    fn get_followees_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor> {
        get_cursor(self, self_id, FOLLOWEES_LAST_SEEN_COOKIE_NAME)
    }

    fn save_followees_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    ) {
        save_cursor(self, self_id, FOLLOWEES_LAST_SEEN_COOKIE_NAME, pagination);
    }

    fn get_network_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor> {
        get_cursor(self, self_id, NETWORK_LAST_SEEN_COOKIE_NAME)
    }

    fn save_network_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    ) {
        save_cursor(self, self_id, NETWORK_LAST_SEEN_COOKIE_NAME, pagination);
    }

    fn get_shoutbox_last_seen(
        &self,
        self_id: impl Into<ShortRostraId>,
    ) -> Option<ReceivedAtPaginationCursor> {
        get_cursor(self, self_id, SHOUTBOX_LAST_SEEN_COOKIE_NAME)
    }

    fn save_shoutbox_last_seen(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        pagination: ReceivedAtPaginationCursor,
    ) {
        save_cursor(self, self_id, SHOUTBOX_LAST_SEEN_COOKIE_NAME, pagination);
    }

    fn get_persona(&self, self_id: impl Into<ShortRostraId>) -> Option<u8> {
        let self_id = self_id.into();
        if let Some(s) = self.get(&format!("{self_id}-{PERSONA_COOKIE_NAME}")) {
            s.value()
                .parse::<u8>()
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
        cookie.set_path("/");
        cookie.set_max_age(time::Duration::weeks(50));
        self.add(cookie);
    }
}
