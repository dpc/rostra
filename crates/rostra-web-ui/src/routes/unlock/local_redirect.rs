use std::fmt;

use axum::http::uri::PathAndQuery;

/// A validated local path and optional query used after authentication.
#[derive(Clone)]
pub(crate) struct LocalRedirect(PathAndQuery);

impl LocalRedirect {
    /// Parse a redirect that cannot be interpreted as an external URL.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        if value.contains('\\') {
            return None;
        }
        let parsed = value.parse::<PathAndQuery>().ok()?;
        let path = parsed.path();
        if !path.starts_with('/') || path.starts_with("//") {
            return None;
        }
        Some(Self(parsed))
    }

    /// Return the validated path and query.
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for LocalRedirect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
