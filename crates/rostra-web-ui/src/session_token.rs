//! Session token type for identifying browser sessions.
//!
//! This module provides a newtype wrapper for session tokens, used to key
//! in-memory secret storage and prevent accidental misuse of raw i128 values.
//!
//! The token is derived from tower-sessions' session ID, which is available
//! after a session has been saved to the store.

use tower_sessions::Session;

/// A unique token identifying a browser session.
///
/// This wraps the tower-sessions session ID (i128), used to key in-memory
/// secret storage. Using a newtype instead of raw i128 provides type safety
/// and prevents accidental misuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionToken(i128);

impl SessionToken {
    /// Create a session token from a tower-sessions Session.
    ///
    /// Returns `None` if the session doesn't have an ID yet (e.g., new session
    /// that hasn't been saved to the store).
    pub fn from_session(session: &Session) -> Option<Self> {
        session.id().map(|id| Self(id.0))
    }

    /// Get the underlying i128 value.
    ///
    /// This is intentionally not public outside the crate to prevent
    /// accidental misuse of the raw value.
    pub(crate) fn as_i128(self) -> i128 {
        self.0
    }
}
