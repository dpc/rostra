mod add_followee;
mod avatar;
mod content;
mod cookies;
mod feeds;
pub mod fragment;
mod media;
mod new_post;
mod post;
mod profile;
mod profile_self;
mod search;
mod settings;
mod timeline;
mod unlock;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Request, State};
use axum::http::header::{self, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_dpc_static_assets::handle_etag;
use maud::Markup;

use super::SharedState;
use super::error::{RequestError, UserErrorResponse};
use crate::UiState;

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
            self.0.0,
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

pub async fn cache_control(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;

    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        const NON_CACHEABLE_CONTENT_TYPES: &[&str] = &["text/html"];
        const SHORT_CACHE_CONTENT_TYPES: &[&str] = &["text/css"];

        let cache_duration_secs = if SHORT_CACHE_CONTENT_TYPES
            .iter()
            .any(|&ct| content_type.as_bytes().starts_with(ct.as_bytes()))
        {
            Some(10 * 60)
        } else if NON_CACHEABLE_CONTENT_TYPES
            .iter()
            .any(|&ct| content_type.as_bytes().starts_with(ct.as_bytes()))
        {
            None
        } else {
            Some(60 * 60)
        };

        if let Some(dur) = cache_duration_secs {
            let value = format!("public, max-age={dur}");

            response.headers_mut().insert(
                "cache-control",
                HeaderValue::from_str(&value).expect("Can't fail"),
            );
        }
    }

    response
}

pub async fn get_static_asset(
    state: State<SharedState>,
    Path(path): Path<String>,
    req_headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(assets) = &state.assets {
        if let Some(asset) = assets.get(&path) {
            let mut resp_headers = HeaderMap::new();

            // Set content type
            if let Some(content_type) = asset.content_type() {
                resp_headers.insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static(content_type),
                );
            }

            // Handle ETag and conditional request
            if let Some(response) = handle_etag(&req_headers, &asset.etag, &mut resp_headers) {
                return response;
            }

            let accepts_brotli = req_headers
                .get_all(axum::http::header::ACCEPT_ENCODING)
                .into_iter()
                .any(|encodings| {
                    let Ok(str) = encodings.to_str() else {
                        return false;
                    };

                    str.split(',').any(|s| s.trim() == "br")
                });

            let content = match (accepts_brotli, asset.compressed.as_ref()) {
                (true, Some(compressed)) => {
                    resp_headers.insert(
                        axum::http::header::CONTENT_ENCODING,
                        axum::http::HeaderValue::from_static("br"),
                    );
                    compressed.clone()
                }
                _ => asset.raw.clone(),
            };

            return (resp_headers, content).into_response();
        }
    }

    axum::http::StatusCode::NOT_FOUND.into_response()
}

pub async fn not_found(_state: State<SharedState>, _req: Request<Body>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        AppJson(UserErrorResponse {
            message: "Not Found".to_string(),
        }),
    )
}

pub fn route_handler(state: SharedState) -> Router<Arc<UiState>> {
    Router::new()
        .route("/", get(root))
        .route("/ui", get(timeline::get_followees))
        .route("/ui/followees", get(timeline::get_followees))
        .route("/ui/network", get(timeline::get_network))
        .route("/ui/notifications", get(timeline::get_notifications))
        .route("/ui/profile/{id}", get(profile::get_profile))
        .route(
            "/ui/profile/{id}/atom.xml",
            get(feeds::get_profile_feed_atom),
        )
        .route(
            "/ui/profile/{id}/follow",
            get(profile::get_follow_dialog).post(profile::post_follow),
        )
        .route("/ui/avatar/{id}", get(avatar::get))
        .route("/ui/media/{author}/{event_id}", get(media::get))
        .route("/ui/media/{author}/list", get(media::list))
        .route(
            "/ui/media/publish",
            post(media::publish).layer(DefaultBodyLimit::max(9_000_000)),
        )
        .route("/ui/updates", get(timeline::get_updates))
        .route("/ui/post/{author}/{event}", get(post::get_single_post))
        .route(
            "/ui/post/{post_thread_id}/{author}/{event}/fetch",
            post(post::fetch_missing_post).get(post::fetch_missing_post),
        )
        .route("/ui/post/{author}/{event}/delete", post(post::delete_post))
        .route("/ui/post", post(new_post::post_new_post))
        .route("/ui/post/preview", post(new_post::get_post_preview))
        .route(
            "/ui/post/preview_dialog",
            post(new_post::get_post_preview_dialog),
        )
        .route("/ui/post/reply_to", get(new_post::get_reply_to))
        .route("/ui/followee", post(add_followee::add_followee))
        .route("/ui/unlock", get(unlock::get).post(unlock::post_unlock))
        .route("/ui/unlock/logout", get(unlock::get).post(unlock::logout))
        .route("/ui/unlock/random", get(unlock::get_random))
        .route(
            "/ui/comments/{post_thread_id}/{event_id}",
            get(timeline::get_post_comments),
        )
        .route(
            "/ui/self/edit",
            get(profile_self::get_self_account_edit).post(profile_self::post_self_account_edit),
        )
        .route("/ui/search/profiles", get(search::search_profiles))
        .route("/ui/settings", get(settings::get_settings))
        .route(
            "/ui/settings/following",
            get(settings::get_settings_following),
        )
        .route(
            "/ui/settings/followers",
            get(settings::get_settings_followers),
        )
        .route("/ui/settings/events", get(settings::get_settings_events))
        .route("/ui/settings/p2p", get(settings::get_settings_p2p))
        .route("/ui/timeline/prime", get(timeline_prime))
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

/// Returns empty timeline-posts div for priming alpine-ajax.
///
/// Workaround: The first alpine-ajax request on a page causes the browser to
/// scroll to the top. By triggering a dummy ajax request on page load (when
/// we're already at the top), the first real infinite scroll request won't
/// cause the unwanted scroll jump.
async fn timeline_prime() -> Maud {
    Maud(maud::html! {
        div id="timeline-posts" x-merge="append" {}
    })
}
