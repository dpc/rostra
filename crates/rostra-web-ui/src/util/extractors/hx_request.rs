use std::ops;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// Extractor that returns `true` if the `HX-Request` header is present and
/// equals "true"
#[derive(Debug, Clone, Copy)]
pub struct HxRequest(pub bool);

impl HxRequest {
    /// Returns `true` if this is an HTMX request
    pub fn is_htmx_request(&self) -> bool {
        self.0
    }
}

impl<S> FromRequestParts<S> for HxRequest
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let is_htmx = parts
            .headers
            .get("HX-Request")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        Ok(HxRequest(is_htmx))
    }
}

impl From<HxRequest> for bool {
    fn from(hx_request: HxRequest) -> bool {
        hx_request.0
    }
}

impl ops::Deref for HxRequest {
    type Target = bool;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
