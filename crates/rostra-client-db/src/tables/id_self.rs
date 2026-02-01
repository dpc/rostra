//! Local account information stored in the `ids_self` table.

use bincode::{Decode, Encode};
use rostra_core::id::RostraId;

/// Record for the `ids_self` table - information about the local user's
/// account.
///
/// This is a singleton table (key is `()`) that stores the local user's
/// identity and network credentials.
#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct IdSelfAccountRecord {
    /// The local user's Rostra identity (public key)
    pub rostra_id: RostraId,
    /// Secret key for the iroh network identity.
    ///
    /// This is separate from the Rostra identity secret key - it's used
    /// specifically for the iroh p2p transport layer.
    pub iroh_secret: [u8; 32],
}
