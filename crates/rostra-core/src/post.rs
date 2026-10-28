//! Exact root-relative public post references, without routing authority.
//!
//! This intentionally accepts less than a web router: no URL decoding, origin
//! inference, aliases, query, fragment, or suffix actions. A full event must
//! match a retained signed envelope before conversion to a short navigation ID.

use std::fmt;
use std::str::FromStr;

use crate::id::{RostraId, ShortRostraId};
use crate::{EventId, ShortEventId};

#[cfg(test)]
mod tests;

/// `/post/`, a full `rs`-prefixed identity, `/`, and a full base32 event ID.
const MAX_POST_REFERENCE_BYTES: usize = 6 + 54 + 1 + 52;

/// An exact identity component, not evidence that the author is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostAuthorReference {
    /// Complete canonical identity.
    Full(RostraId),
    /// Canonical prefix requiring retained identity-index resolution.
    Short(ShortRostraId),
}

/// An exact event component; full IDs must never be resolved by prefix alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostEventReference {
    /// Complete signed event hash.
    Full(EventId),
    /// Author-scoped retained event key.
    Short(ShortEventId),
}

/// A syntactic `/post/{author}/{event}` reference, never a fetch instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostReference {
    /// Requested author, independent of any displayed link label.
    pub author: PostAuthorReference,
    /// Requested event, preserving full versus short identity.
    pub event: PostEventReference,
}

/// The input is outside the exact canonical root-relative post grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPostReference;

impl fmt::Display for InvalidPostReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an exact canonical root-relative post reference")
    }
}

impl std::error::Error for InvalidPostReference {}

impl FromStr for PostReference {
    type Err = InvalidPostReference;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Bound even direct callers before allocating in component decoders.
        if value.len() > MAX_POST_REFERENCE_BYTES {
            return Err(InvalidPostReference);
        }
        let (author, event) = value
            .strip_prefix("/post/")
            .and_then(|path| path.split_once('/'))
            .ok_or(InvalidPostReference)?;
        let reference = Self {
            author: author
                .parse()
                .map(PostAuthorReference::Full)
                .or_else(|_| author.parse().map(PostAuthorReference::Short))
                .map_err(|_| InvalidPostReference)?,
            event: event
                .parse()
                .map(PostEventReference::Full)
                .or_else(|_| event.parse().map(PostEventReference::Short))
                .map_err(|_| InvalidPostReference)?,
        };
        // General identity decoders accept legacy aliases and noncanonical
        // padding bits. Native post paths deliberately do not.
        (reference.to_string() == value)
            .then_some(reference)
            .ok_or(InvalidPostReference)
    }
}

impl fmt::Display for PostAuthorReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(id) => id.fmt(f),
            Self::Short(id) => id.fmt(f),
        }
    }
}

impl fmt::Display for PostEventReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(id) => id.fmt(f),
            Self::Short(id) => id.fmt(f),
        }
    }
}

impl fmt::Display for PostReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/post/{}/{}", self.author, self.event)
    }
}
