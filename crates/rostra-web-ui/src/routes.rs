mod add_followee;
pub(crate) mod api;
mod avatar;
mod content;
mod cookies;
mod debug;
mod feeds;
pub mod fragment;
mod media;
mod new_post;
mod post;
mod profile;
pub(crate) mod profile_self;
mod search;
mod settings;
mod shoutbox;
mod timeline;
pub(crate) mod unlock;
mod welcome;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::header::{self, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
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
    let path = request.uri().path().to_owned();

    let mut response = next.run(request).await;

    // Avatars: long cache, busted by ?v= query param on URL changes
    if path.starts_with("/profile/") && path.ends_with("/avatar") {
        response.headers_mut().insert(
            "cache-control",
            HeaderValue::from_static("public, max-age=86400"),
        );
        return response;
    }

    // Check if this is a static asset request (always cacheable)
    let is_static_asset = path.starts_with("/assets/");

    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        const NON_CACHEABLE_CONTENT_TYPES: &[&str] = &["text/html", "application/json"];
        const SHORT_CACHE_CONTENT_TYPES: &[&str] = &["text/css"];

        let cache_duration_secs = if is_static_asset {
            // Static assets are always cacheable
            Some(60 * 60)
        } else if SHORT_CACHE_CONTENT_TYPES
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
        .nest("/api", api::api_router())
        .route("/", get(welcome::get_landing))
        .route("/home", get(welcome::get_home))
        .route("/followees", get(timeline::get_followees))
        .route("/network", get(timeline::get_network))
        .route("/notifications", get(timeline::get_notifications))
        .route("/profile/{id}", get(profile::get_profile))
        .route("/profile/{id}/atom.xml", get(feeds::get_profile_feed_atom))
        .route(
            "/profile/{id}/follow",
            get(profile::get_follow_dialog).post(profile::post_follow),
        )
        .route("/profile/{id}/avatar", get(avatar::get))
        .route("/media/{author}/{event_id}", get(media::get))
        .route("/media/{author}/list", get(media::list))
        .route(
            "/media/publish",
            post(media::publish).layer(DefaultBodyLimit::max(200_000_000)),
        )
        .route("/updates", get(timeline::get_updates))
        .route("/post/{author}/{event}", get(post::get_single_post))
        .route(
            "/post/{post_thread_id}/{author}/{event}/fetch",
            post(post::fetch_missing_post).get(post::fetch_missing_post),
        )
        .route("/post/{author}/{event}/delete", post(post::delete_post))
        .route("/post", post(new_post::post_new_post))
        .route(
            "/post/new_post_preview",
            post(new_post::get_new_post_preview),
        )
        .route(
            "/post/preview_dialog",
            post(new_post::post_post_preview_dialog),
        )
        .route("/post/inline_reply", get(new_post::get_inline_reply))
        .route(
            "/post/inline_reply_cancel",
            get(new_post::get_inline_reply_cancel),
        )
        .route(
            "/post/inline_reply_preview",
            post(new_post::post_inline_reply_preview),
        )
        .route("/followee", post(add_followee::add_followee))
        .route("/shoutbox", get(shoutbox::get_shoutbox))
        .route("/shoutbox/post", post(shoutbox::post_shoutbox))
        .route("/unlock", get(unlock::get).post(unlock::post_unlock))
        .route("/unlock/logout", get(unlock::get).post(unlock::logout))
        .route("/unlock/random", get(unlock::get_random))
        .route(
            "/replies/{post_thread_id}/{event_id}",
            get(timeline::get_post_replies),
        )
        .route("/self/edit", post(profile_self::post_self_account_edit))
        .route("/search/profiles", get(search::search_profiles))
        .route("/settings", get(settings::get_settings))
        .route(
            "/settings/profile",
            get(settings::get_settings_profile).post(settings::post_settings_profile),
        )
        .route(
            "/settings/profile/preview",
            post(settings::post_settings_profile_preview),
        )
        .route("/settings/following", get(settings::get_settings_following))
        .route("/settings/followers", get(settings::get_settings_followers))
        .route("/settings/events", get(settings::get_settings_events))
        .route("/settings/p2p", get(settings::get_settings_p2p))
        // .route("/a/", put(account_new))
        // .route("/t/", put(token_new))
        // .route("/m/", put(metric_new).get(metric_find))
        // .route("/m/:metric", post(metric_post).get(metric_get_default_type))
        // .route("/m/:metric/:type", get(metric_get))
        .fallback(not_found)
        .with_state(state)
}
