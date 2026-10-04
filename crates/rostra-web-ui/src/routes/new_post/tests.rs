use std::collections::BTreeSet;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use axum::response::Html as AxumHtml;
use axum::routing::{get, post};
use rostra_core::event::PersonaTag;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};
use scraper::{Html, Selector};
use tower::ServiceExt as _;
use tower_cookies::{CookieManagerLayer, Cookies};

use super::{RoMode, UiState, selected_persona_tags};
use crate::routes::cookies::CookiesExt as _;
use crate::routes::fragment;

fn cookie_test_id() -> RostraId {
    RostraId::from_bytes([42; 32])
}

fn transport_test_tags() -> BTreeSet<PersonaTag> {
    [
        "personal",
        "professional",
        "a%20b",
        "a%22b",
        "a;b",
        "%",
        "%2520",
        "say\"hi\\there",
        "ünicode",
    ]
    .into_iter()
    .map(|tag| PersonaTag::new(tag.to_owned()).unwrap())
    .collect()
}

async fn save_transport_test_tags(mut cookies: Cookies) {
    cookies.save_persona_tags(cookie_test_id(), &transport_test_tags());
}

async fn read_transport_test_tags(cookies: Cookies) -> AxumHtml<String> {
    let tags = cookies.get_persona_tags(cookie_test_id());
    AxumHtml(
        serde_json::to_string(&tags.iter().map(PersonaTag::as_str).collect::<Vec<_>>()).unwrap(),
    )
}

async fn render_nojs_persona_selector(cookies: Cookies) -> AxumHtml<String> {
    let selected_tags = selected_persona_tags(&cookies, cookie_test_id());
    let mut available_tags = transport_test_tags();
    available_tags.extend(PersonaTag::defaults());
    AxumHtml(
        fragment::persona_tag_select("persona_tags")
            .available_tags(&available_tags)
            .selected_tags(&selected_tags)
            .id("post-persona-tags-nojs")
            .call()
            .into_string(),
    )
}

fn cookie_test_app() -> Router {
    Router::new()
        .route(
            "/",
            post(save_transport_test_tags).get(read_transport_test_tags),
        )
        .route("/composer", get(render_nojs_persona_selector))
        .layer(CookieManagerLayer::new())
}

fn persona_tags_cookie_name() -> String {
    format!("{}-persona-tags", cookie_test_id().to_short())
}

async fn response_body(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn checked_persona_tags(body: &str) -> BTreeSet<String> {
    let document = Html::parse_fragment(body);
    document
        .select(&Selector::parse("input[name=\"persona_tags\"][checked]").unwrap())
        .map(|input| input.value().attr("value").unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn persona_tag_cookie_round_trips_through_cookie_manager() {
    let app = cookie_test_app();
    let save_response = app
        .clone()
        .oneshot(Request::post("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(save_response.status(), StatusCode::OK);

    let set_cookie = save_response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    let cookie_pair = set_cookie.split_once(';').unwrap().0;
    let value = cookie_pair.split_once('=').unwrap().1;
    assert!(!value.contains([';', '"', ' ', '\\', '\t', '\r', '\n']));
    assert!(value.is_ascii());
    assert!(set_cookie.contains("; Path=/"));
    assert!(set_cookie.contains("; Max-Age=30240000"));

    let read_response = app
        .clone()
        .oneshot(
            Request::get("/")
                .header(COOKIE, cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let expected: Vec<String> = transport_test_tags()
        .into_iter()
        .map(|tag| tag.to_string())
        .collect();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&response_body(read_response).await).unwrap(),
        expected,
    );

    let composer_response = app
        .oneshot(
            Request::get("/composer")
                .header(COOKIE, cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        checked_persona_tags(&response_body(composer_response).await),
        transport_test_tags()
            .into_iter()
            .map(|tag| tag.to_string())
            .collect()
    );
}

#[tokio::test]
async fn legacy_persona_tag_cookies_keep_existing_read_and_fallback_behavior() {
    let app = cookie_test_app();
    let cookie_name = persona_tags_cookie_name();
    let legacy_response = app
        .clone()
        .oneshot(
            Request::get("/")
                .header(
                    COOKIE,
                    format!(r#"{cookie_name}=["personal","professional"]"#),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&response_body(legacy_response).await).unwrap(),
        ["personal", "professional"],
    );

    let malformed_response = app
        .oneshot(
            Request::get("/composer")
                .header(COOKIE, format!("{cookie_name}=not-json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        checked_persona_tags(&response_body(malformed_response).await),
        BTreeSet::from(["personal".to_owned()])
    );
}

fn control(document: &Html, selector: &str, label: &str) {
    let control = document
        .select(&Selector::parse(selector).unwrap())
        .next()
        .unwrap_or_else(|| panic!("missing control {selector}"));
    assert_eq!(control.value().attr("title"), Some(label));
    assert_eq!(control.value().attr("aria-label"), Some(label));
}

fn shortcut_submit(document: &Html, button_selector: &str, title: &str, textarea_selector: &str) {
    let button = document
        .select(&Selector::parse(button_selector).unwrap())
        .next()
        .unwrap_or_else(|| panic!("missing button {button_selector}"));
    assert_eq!(button.value().attr("title"), Some(title));

    let textarea = document
        .select(&Selector::parse(textarea_selector).unwrap())
        .next()
        .unwrap_or_else(|| panic!("missing textarea {textarea_selector}"));
    assert!(textarea.value().attr("x-on:keyup.enter.ctrl").is_some());
}

#[test]
fn formatting_help_visibility_does_not_follow_textarea_focus() {
    let stylesheet = include_str!("../../../assets/style.css");
    let visibility_rule = stylesheet
        .split_once(".m-newPostForm__textareaWrapper:hover")
        .unwrap()
        .1
        .split_once('}')
        .unwrap()
        .0;
    let selectors = visibility_rule.split_once('{').unwrap().0;
    assert!(selectors.contains(".m-newPostForm__helpButton--inset:focus-visible"));
    assert!(!selectors.contains(":focus-within"));
    assert!(visibility_rule.contains("opacity: 1"));
    assert!(stylesheet.contains("@media (hover: none)"));
    assert!(stylesheet.contains("@media (prefers-reduced-motion: reduce)"));
}

#[test]
fn post_composer_icon_controls_have_tooltips_and_accessible_names() {
    let self_id = RostraId::from_bytes([42; 32]);
    let inline_reply = Html::parse_fragment(
        &UiState::render_inline_reply_form(
            ExternalEventId::new(self_id, ShortEventId::from_bytes([43; 16])),
            ShortEventId::from_bytes([44; 16]),
            self_id,
            RoMode::Rw,
        )
        .into_string(),
    );
    for (selector, label) in [
        (".m-inlineReply__helpButton", "Formatting help"),
        (".m-inlineReply__emojiButton", "Insert emoji"),
        (".m-inlineReply__attachButton", "Attach media"),
        (".m-inlineReply__cancelButton", "Cancel"),
    ] {
        control(&inline_reply, selector, label);
    }
    shortcut_submit(
        &inline_reply,
        ".m-inlineReply__previewButton",
        "Preview reply (Ctrl+Enter)",
        ".m-inlineReply__content",
    );

    let news_post = Html::parse_fragment(
        &UiState::news_post_form_inner(RoMode::Rw, Some(self_id), false).into_string(),
    );
    for (selector, label) in [
        (".m-newPostForm__helpButton", "Formatting help"),
        (".m-newPostForm__emojiButton", "Insert emoji"),
    ] {
        control(&news_post, selector, label);
    }
    shortcut_submit(
        &news_post,
        ".m-newPostForm__previewButton",
        "Preview post (Ctrl+Enter)",
        ".m-newPostForm__content",
    );

    let new_post = Html::parse_fragment(
        &UiState::new_post_form_inner(RoMode::Rw, Some(self_id), false).into_string(),
    );
    for (selector, label) in [
        (".m-newPostForm__helpButton", "Formatting help"),
        (".m-newPostForm__emojiButton", "Insert emoji"),
        (".m-newPostForm__attachButton", "Attach media"),
        (".m-newPostForm__linkButton", "Add link"),
    ] {
        control(&new_post, selector, label);
    }
    shortcut_submit(
        &new_post,
        ".m-newPostForm__previewButton",
        "Preview post (Ctrl+Enter)",
        ".m-newPostForm__content",
    );
}
