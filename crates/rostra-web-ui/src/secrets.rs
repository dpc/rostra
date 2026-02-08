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

use std::collections::BTreeMap;
use std::sync::RwLock;

use rostra_core::id::RostraIdSecretKey;
use tower_sessions_core::session::Id as SessionId;
use tower_sessions_core::session_store::SessionStore;

use crate::SessionToken;

/// Secure in-memory storage for secret keys.
///
/// All fields are private, and the only way to retrieve a secret is by
/// providing the [`SessionToken`]. This prevents any accidental mismatch
/// between sessions and secrets.
///
/// Uses `BTreeMap` for ordered storage, enabling efficient neighbor-based
/// garbage collection.
pub struct SecretStore {
    /// Map from session token to secret key (ordered for GC efficiency).
    secrets: RwLock<BTreeMap<i128, RostraIdSecretKey>>,
}

impl SecretStore {
    /// Create a new empty secret store.
    pub fn new() -> Self {
        Self {
            secrets: RwLock::new(BTreeMap::new()),
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

    /// Remove secrets for sessions that no longer exist in the session store.
    ///
    /// Uses incremental GC: only checks the immediate neighbors (one before,
    /// one after) of the given session token. This bounds work to at most 2
    /// session store lookups per login, while gradually cleaning up expired
    /// secrets over time as new sessions are created.
    pub async fn gc(&self, session_token: SessionToken, session_store: &impl SessionStore) {
        let current = session_token.as_i128();

        // Find neighbor keys (one before, one after the current session)
        let (prev_key, next_key) = {
            let secrets = self.secrets.read().unwrap();
            let prev = secrets.range(..current).next_back().map(|(&k, _)| k);
            let next = secrets
                .range((
                    std::ops::Bound::Excluded(current),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .map(|(&k, _)| k);
            (prev, next)
        };

        // Check and remove expired neighbors
        for key in [prev_key, next_key].into_iter().flatten() {
            let session_id = SessionId(key);
            if session_store
                .load(&session_id)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                self.secrets.write().unwrap().remove(&key);
            }
        }
    }
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}
