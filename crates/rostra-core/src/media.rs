//! References to signed media, without retrieval or presentation authority.

use crate::{EventId, ShortEventId};

/// A media reference still requiring retained-envelope and source-author
/// checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaEventReference {
    /// A complete ID; a matching short prefix alone is insufficient.
    Full(EventId),
    /// An unresolved author-scoped retained-event key.
    Short(ShortEventId),
}

impl MediaEventReference {
    /// Return the lookup key, not proof that the retained event matches.
    pub fn short_id(self) -> ShortEventId {
        match self {
            Self::Full(id) => id.into(),
            Self::Short(id) => id,
        }
    }
}

impl std::str::FromStr for MediaEventReference {
    type Err = data_encoding::DecodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self::Full)
            .or_else(|_| value.parse().map(Self::Short))
    }
}

impl std::fmt::Display for MediaEventReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full(id) => id.fmt(f),
            Self::Short(id) => id.fmt(f),
        }
    }
}
