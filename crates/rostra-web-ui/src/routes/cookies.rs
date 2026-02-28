use std::collections::BTreeSet;

use rostra_client_db::social::ReceivedAtPaginationCursor;
use rostra_core::event::PersonaTag;
use rostra_core::id::ShortRostraId;
use rostra_util_error::FmtCompact as _;
use tower_cookies::{Cookie, Cookies};
use tracing::debug;

use crate::LOG_TARGET;

const NOTIFICATIONS_LAST_SEEN_COOKIE_NAME: &str = "notifications-last-seen";
const FOLLOWEES_LAST_SEEN_COOKIE_NAME: &str = "followees-last-seen";
const NETWORK_LAST_SEEN_COOKIE_NAME: &str = "network-last-seen";
const SHOUTBOX_LAST_SEEN_COOKIE_NAME: &str = "shoutbox-last-seen";
const PERSONA_TAGS_COOKIE_NAME: &str = "persona-tags";

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

    fn get_persona_tags(&self, self_id: impl Into<ShortRostraId>) -> BTreeSet<PersonaTag>;

    fn save_persona_tags(&mut self, self_id: impl Into<ShortRostraId>, tags: &BTreeSet<PersonaTag>);
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

    fn get_persona_tags(&self, self_id: impl Into<ShortRostraId>) -> BTreeSet<PersonaTag> {
        let self_id = self_id.into();
        if let Some(s) = self.get(&format!("{self_id}-{PERSONA_TAGS_COOKIE_NAME}")) {
            let tag_strings: Vec<String> = serde_json::from_str(s.value())
                .inspect_err(|err| {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Invalid persona-tags cookie value");
                })
                .unwrap_or_default();
            tag_strings
                .into_iter()
                .filter_map(|s| PersonaTag::new(s).ok())
                .collect()
        } else {
            BTreeSet::new()
        }
    }

    fn save_persona_tags(
        &mut self,
        self_id: impl Into<ShortRostraId>,
        tags: &BTreeSet<PersonaTag>,
    ) {
        let self_id = self_id.into();
        let tag_strings: Vec<&str> = tags.iter().map(|t| t.as_str()).collect();
        let mut cookie = Cookie::new(
            format!("{self_id}-{PERSONA_TAGS_COOKIE_NAME}"),
            serde_json::to_string(&tag_strings).expect("can't fail"),
        );
        cookie.set_path("/");
        cookie.set_max_age(time::Duration::weeks(50));
        self.add(cookie);
    }
}
