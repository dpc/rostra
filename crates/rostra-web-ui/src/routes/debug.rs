//! Debug utilities for the web UI.
//!
//! This module provides debugging helpers that can be enabled via environment
//! variables. When enabled, debug information is displayed in the UI to help
//! diagnose issues.
//!
//! # Environment Variables
//!
//! - `ROSTRA_DEBUG_NOTIFICATIONS`: When set to "1" or "true", displays
//!   notification counting debug information in the timeline header.

use std::sync::LazyLock;

use maud::{Markup, html};
use rostra_client_db::social::ReceivedAtPaginationCursor;

use super::timeline::TimelineMode;

/// Check if notification debugging is enabled via environment variable.
///
/// Set `ROSTRA_DEBUG_NOTIFICATIONS=1` or `ROSTRA_DEBUG_NOTIFICATIONS=true`
/// to enable debug output for notification counting.
pub fn notifications_debug_enabled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| {
        std::env::var("ROSTRA_DEBUG_NOTIFICATIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });
    *ENABLED
}

/// Debug information for notification counting.
///
/// Captures the state of notification counting for display in the UI
/// when debugging is enabled.
#[derive(Debug, Default)]
pub struct NotificationDebugInfo {
    /// The current timeline mode.
    pub mode: Option<TimelineMode>,
    /// The cursor of the most recent post in the database.
    pub latest_cursor: Option<ReceivedAtPaginationCursor>,
}

impl NotificationDebugInfo {
    /// Create debug info for when saving the last-seen cursor.
    pub fn for_save(mode: TimelineMode, latest_cursor: Option<ReceivedAtPaginationCursor>) -> Self {
        Self {
            mode: Some(mode),
            latest_cursor,
        }
    }

    /// Render the debug info as HTML.
    ///
    /// Returns empty markup if debugging is disabled or no info is available.
    pub fn render(&self) -> Markup {
        if !notifications_debug_enabled() || self.mode.is_none() {
            return html! {};
        }

        let content = format!(
            "MODE={:?}, latest_cursor={:?}, saving to cookie",
            self.mode.unwrap(),
            self.latest_cursor
        );

        html! {
            div
                style="background: #ff0; color: #000; padding: 8px; font-family: monospace; font-size: 12px; white-space: pre-wrap; word-break: break-all;"
            {
                (content)
            }
        }
    }
}
