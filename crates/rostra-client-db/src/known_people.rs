//! Bounded local self/direct-followee/two-hop search, without network
//! discovery.
//!
//! Each call reads one snapshot; continuation is **live**, not a snapshot
//! across calls. Inserts behind a cursor can be missed and removed/changed
//! candidates can remain in the best-so-far results. Refresh to reconcile.
//! Exhaustion only means traversal ended: it does not promise a consistent
//! global top ten under mutation, nor web-ranking parity when profile names
//! were unavailable.
//!
//! Cursors are opaque in-process values, not serializable client input or
//! durable capabilities. Hosts retain them in a bounded session slot and
//! discard them on account/query changes. Reopening or rebuilding the
//! projection rejects them. No schema, ingestion, fetch scheduling, watch,
//! signing or network changes.

use rostra_core::Timestamp;
use rostra_core::id::RostraId;

mod profile;
mod read;
mod score;
pub use score::fuzzy_match;
#[cfg(test)]
mod tests;

/// Maximum original query size, before Unicode lowercase expansion.
pub const QUERY_BYTES_MAX: usize = 256;
/// Maximum owned display-name size (UTF-8 bytes).
pub const NAME_BYTES_MAX: usize = 1024;
/// Maximum raw profile admitted to borrowed shape validation.
pub const PROFILE_BYTES_MAX: usize = 2 * 1024 * 1024;
/// Maximum profile observations per call, including duplicate identities.
pub const PROFILES_MAX: usize = 8;
/// Maximum graph iterator advances per call, including exhausted ranges.
pub const GRAPH_STEPS_MAX: usize = 128;
/// Maximum unique cumulative ranked results.
pub const RESULTS_MAX: usize = 10;

/// Why a projected metadata field is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileIssue {
    /// No profile projection exists; the normal web fallback applies.
    Missing,
    /// Raw profile or display name exceeds this reader's presentation policy.
    Oversized,
    /// Invalid stored encoding, including trailing bytes.
    Corrupt,
    /// The projection's source envelope is not retained.
    MissingSource,
    /// The retained source does not match author, kind, timestamp or event ID.
    MismatchedSource,
}

/// Independent field availability, never an identity-membership judgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileField<T> {
    /// Bounded local projected value.
    Available(T),
    /// Missing or unusable value, with an explicit reason.
    Unavailable(ProfileIssue),
}

/// Bounded projected metadata, not a re-verification of signed profile content.
///
/// The retained source envelope is checked, but the projection's display name
/// is not compared with its original body. No bio/avatar is copied or returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonProfile {
    /// Name and timestamp availability are independent.
    pub display_name: ProfileField<String>,
    /// Timestamp from the matching retained source envelope.
    pub timestamp: ProfileField<Timestamp>,
}

/// One full-identity ranked result; display fallback is not profile
/// availability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownPerson {
    /// Exact full identity from the graph, independent of the short-ID index.
    pub identity: RostraId,
    /// Available name, otherwise the web's first-eight-ID-characters fallback.
    pub display_name: String,
    /// Field availability and bounded projected metadata.
    pub profile: PersonProfile,
    /// Maximum subsequence score against lowercased name/fallback and full ID.
    pub score: i32,
}

/// Whether graph traversal has ended (not a snapshot or ranking-quality claim).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStop {
    /// More bounded work remains; even an empty result page can continue.
    Incomplete,
    /// All ranges in this live traversal ended.
    Exhausted,
}

/// A bounded cumulative top ten, replacing rather than appending prior results.
#[derive(Debug)]
pub struct KnownPeoplePage {
    /// Unique full IDs ordered by descending score, then ascending full ID.
    pub rows: Vec<KnownPerson>,
    /// Present exactly when stop is Incomplete.
    pub next_cursor: Option<KnownPeopleCursor>,
    /// Traversal completion independent of profile coverage.
    pub stop: SearchStop,
    /// Cumulative unavailable-name observations, excluding normal missing
    /// profiles.
    ///
    /// Counts observations, not distinct people; duplicates may be counted
    /// again. A nonzero count means even stable-graph exhausted results can
    /// differ from the web's unbounded ranking. This warning survives
    /// omitted candidates.
    pub unscorable_profiles: u64,
    /// Raw graph advances in this call, including empty range probes.
    pub graph_steps: usize,
    /// Profile point reads in this call, including missing and duplicate
    /// profiles.
    pub profiles_read: usize,
}

/// Exclusive live search position with bounded cumulative ranking state.
#[derive(Clone, Debug)]
pub struct KnownPeopleCursor {
    /// Account/open/projection binding.
    scope: Scope,
    /// Unicode-lowercased query; bounded before expansion.
    query: String,
    /// Nested graph position, never a visited-ID collection.
    position: Position,
    /// Fixed top ten accumulated across calls.
    best: Vec<KnownPerson>,
    /// Cumulative coverage warning, including candidates outside the top ten.
    unscorable_profiles: u64,
}

/// Membership specifically in the self/direct/two-hop candidate graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphMembership {
    /// The exact full identity was found in this call's graph snapshot.
    Known,
    /// Traversal ended without finding it; not proof the identity does not
    /// exist.
    Unknown,
    /// No membership proof yet; continue rather than treating this as unknown.
    Incomplete,
}

/// Exact local identity evidence, separate from candidate-graph membership.
#[derive(Debug)]
pub struct KnownPersonLookup {
    /// Exact requested full identity.
    pub identity: RostraId,
    /// Self/direct/two-hop evidence, not a short-index approximation.
    pub graph_membership: GraphMembership,
    /// Exact full ID matches the retained event-author index.
    pub retained_identity: bool,
    /// Independent projected profile availability.
    pub profile: PersonProfile,
    /// Present only for incomplete membership traversal.
    pub next_cursor: Option<KnownPersonLookupCursor>,
    /// Outer graph advances, including exhausted-range probes, in this call.
    pub graph_steps: usize,
}

/// Opaque target-bound full-ID membership continuation, distinct from search.
#[derive(Clone, Debug)]
pub struct KnownPersonLookupCursor {
    /// Account/open/projection binding.
    scope: Scope,
    /// Exact requested full ID.
    target: RostraId,
    /// Last direct followee examined.
    after: Option<RostraId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    /// Exact database owner.
    account: RostraId,
    /// Open-instance nonce.
    instance: [u8; 16],
    /// Projection rebuild nonce.
    epoch: [u8; 16],
}

#[derive(Clone, Debug)]
enum Position {
    /// Emit owner once.
    SelfIdentity,
    /// Seek the next direct followee.
    Outer(Option<RostraId>),
    /// Seek the next followee of the current direct followee.
    Inner {
        /// Current direct followee, also the outer resume position.
        owner: RostraId,
        /// Last second-hop candidate examined.
        after: Option<RostraId>,
    },
}
