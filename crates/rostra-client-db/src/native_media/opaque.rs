//! Authenticated retained bytes for explicit opaque export, not presentation.

use rostra_core::EventId;
use rostra_core::id::RostraId;

/// A bounded local snapshot outcome, distinct from database operation failures.
///
/// Available bytes are authenticated, not safe to execute, open or display.
/// The declared MIME is untrusted metadata, not a filename or dispatch policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpaqueMediaRead {
    /// Original bytes passing signed-envelope and complete CBOR checks only.
    Available {
        /// Exact signed media author, equal to the supplied source post author.
        author: RostraId,
        /// Recomputed full signed event ID, even for a short request.
        event_id: EventId,
        /// Original untrusted declaration, at most 64 UTF-8 bytes.
        mime: String,
        /// Original bytes, never rewritten: at most 1,000,000 for the default
        /// reader or 4,000,000 for the explicit opaque-export reader.
        data: Vec<u8>,
    },
    /// No retained envelope or currently processed payload.
    Missing,
    /// Author deletion hides the payload even if shared bytes remain.
    Deleted,
    /// Local retention policy pruned this event's payload.
    Pruned,
    /// Corrupt storage, failed authentication or malformed payload.
    Invalid,
    /// Historical CBOR representations outside the accepted native subset.
    Unsupported,
    /// Signed body, stored body or media byte count exceeds policy.
    TooLarge,
    /// Retained envelope does not match the requested full ID, author or kind.
    Mismatch,
}
