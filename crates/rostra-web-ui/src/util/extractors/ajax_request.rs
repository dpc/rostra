use axum::extract::FromRequestParts;
use axum::http::request::Parts;

pub struct AjaxRequest(pub bool);

impl<S> FromRequestParts<S> for AjaxRequest
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Check for Alpine-ajax request header (X-Alpine-Request: true)
        Ok(AjaxRequest(
            parts
                .headers
                .get("X-Alpine-Request")
                .is_some_and(|value| value == "true"),
        ))
    }
}
