mod add_followee;
mod new_post;
mod self_account;
mod timeline;
mod unlock;

use axum::body::Body;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::header::{self, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use maud::Markup;

use super::error::{RequestError, UserErrorResponse};
use super::SharedState;

#[derive(Clone, Debug)]
#[must_use]
pub struct Maud(pub Markup);

impl IntoResponse for Maud {
    fn into_response(self) -> Response {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            self.0 .0,
        )
            .into_response()
    }
}

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(RequestError))]
pub struct AppJson<T>(pub T);

impl<T> IntoResponse for AppJson<T>
where
    axum::Json<T>: IntoResponse,
{
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

pub fn static_file_handler(state: SharedState) -> Router {
    Router::new()
        .route(
            "/{file}",
            get(
                |state: State<SharedState>, path: Path<String>, req_headers: HeaderMap| async move {
                    let Some(asset) = state.assets.get_from_path(&path) else {
                        return StatusCode::NOT_FOUND.into_response();
                    };

                    let mut resp_headers = HeaderMap::new();

                    // We set the content type explicitly here as it will otherwise
                    // be inferred as an `octet-stream`
                    resp_headers.insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static(
                            asset.content_type().unwrap_or("application/octet-stream"),
                        ),
                    );

                    let accepts_brotli =
                        req_headers
                            .get_all(ACCEPT_ENCODING)
                            .into_iter()
                            .any(|encodings| {
                                let Ok(str) = encodings.to_str() else {
                                    return false;
                                };

                                str.split(',').any(|s| s.trim() == "br")
                            });

                    let content = if accepts_brotli {
                        if let Some(compressed) = asset.compressed.as_ref() {
                            resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static("br"));

                            compressed.clone()
                        } else {
                            asset.raw.clone()
                        }
                    } else {
                        asset.raw.clone()
                    };

                    (resp_headers, content).into_response()
                },
            ),
        )
        .layer(middleware::from_fn(cache_control))
        .with_state(state)
}

pub async fn cache_control(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;

    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        const CACHEABLE_CONTENT_TYPES: &[(&str, u32)] = &[
            ("text/html", 60),
            ("image/svg+xml", 60),
            ("text/css", 60 * 60 * 24),
            ("application/javascript", 60 * 60 * 24),
        ];

        if let Some(&(_, secs)) = CACHEABLE_CONTENT_TYPES
            .iter()
            .find(|&(ct, _)| content_type.as_bytes().starts_with(ct.as_bytes()))
        {
            let value = format!("public, max-age={}", secs);

            if let Ok(value) = HeaderValue::from_str(&value) {
                response.headers_mut().insert("cache-control", value);
            }
        }
    }

    response
}

pub async fn not_found(_state: State<SharedState>, _req: Request<Body>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        AppJson(UserErrorResponse {
            message: "Not Found".to_string(),
        }),
    )
}

pub fn route_handler(state: SharedState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/ui", get(timeline::get))
        .route("/ui/post", post(new_post::new_post))
        .route("/ui/followee", post(add_followee::add_followee))
        .route("/ui/unlock", get(unlock::get).post(unlock::post))
        .route("/ui/unlock/random", get(unlock::get_random))
        // .route("/a/", put(account_new))
        // .route("/t/", put(token_new))
        // .route("/m/", put(metric_new).get(metric_find))
        // .route("/m/:metric", post(metric_post).get(metric_get_default_type))
        // .route("/m/:metric/:type", get(metric_get))
        .fallback(not_found)
        .with_state(state)
        .layer(middleware::from_fn(cache_control))
}

async fn root() -> Redirect {
    Redirect::permanent("/ui")
}
