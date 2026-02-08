//! In-memory secret key storage, keyed by session token.
//!
//! This module provides secure in-memory storage for secret keys. Secrets are
//! intentionally NOT stored in the persistent session store (redb) to avoid
//! persisting them to disk where they could leak.
//!
//! The storage is keyed by [`SessionToken`], ensuring secrets are tied to a
//! specific browser session. This design makes it impossible to accidentally
//! retrieve a secret for the wrong session - you must have the session token
//! to access the secret.

use std::collections::HashMap;
use std::sync::RwLock;

use rostra_core::id::RostraIdSecretKey;

use crate::SessionToken;

/// Secure in-memory storage for secret keys.
///
/// All fields are private, and the only way to retrieve a secret is by
/// providing the [`SessionToken`]. This prevents any accidental mismatch
/// between sessions and secrets.
pub struct SecretStore {
    /// Map from session token to secret key.
    secrets: RwLock<HashMap<i128, RostraIdSecretKey>>,
}

impl SecretStore {
    /// Create a new empty secret store.
    pub fn new() -> Self {
        Self {
            secrets: RwLock::new(HashMap::new()),
        }
    }

    /// Get the secret key for a session.
    ///
    /// Returns `None` if no secret is stored for this session (read-only mode).
    pub fn get(&self, session_token: SessionToken) -> Option<RostraIdSecretKey> {
        self.secrets
            .read()
            .unwrap()
            .get(&session_token.as_i128())
            .copied()
    }

    /// Store a secret key for a session.
    ///
    /// This will overwrite any existing secret for this session token.
    pub fn insert(&self, session_token: SessionToken, secret: RostraIdSecretKey) {
        self.secrets
            .write()
            .unwrap()
            .insert(session_token.as_i128(), secret);
    }

    /// Remove the secret key for a session.
    ///
    /// Used when switching to read-only mode.
    pub fn remove(&self, session_token: SessionToken) {
        self.secrets
            .write()
            .unwrap()
            .remove(&session_token.as_i128());
    }

    /// Check if a session has a secret key stored (is in read-write mode).
    pub fn has_secret(&self, session_token: SessionToken) -> bool {
        self.get(session_token).is_some()
    }
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}
