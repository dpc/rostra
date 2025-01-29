use axum::extract::FromRequestParts;
use axum::http::request::Parts;

pub struct IsHtmx(pub bool);

impl<S> FromRequestParts<S> for IsHtmx
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(IsHtmx(
            parts
                .headers
                .get("HX-Request")
                .is_some_and(|value| value == "true"),
        ))
    }
}
