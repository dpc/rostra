mod common;

use common::TestServer;
use reqwest::header;
use serde_json::json;

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn unauthenticated_landing_page_returns_200() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.get("/").await;
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Rostra"),
        "Landing page should mention Rostra"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn unauthenticated_followees_redirects_to_unlock() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.get("/followees").await;
    assert_eq!(resp.status(), 303);

    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.starts_with("/unlock"),
        "Expected redirect to /unlock, got {location}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn login_then_access_followees() {
    let server = TestServer::start().await;
    let driver = server.driver();

    driver.login_new_identity().await;

    let resp = driver.get("/followees").await;
    assert_eq!(resp.status(), 200);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn preview_empty_post_returns_400() {
    let server = TestServer::start().await;
    let driver = server.driver();

    driver.login_new_identity().await;

    let resp = driver.preview_post("").await;
    assert_eq!(resp.status(), 400);

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Post content cannot be empty"),
        "Expected validation error message in response body, got: {body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn preview_nonempty_post_returns_200() {
    let server = TestServer::start().await;
    let driver = server.driver();

    driver.login_new_identity().await;

    let resp = driver.preview_post("Hello, world!").await;
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Hello, world!"),
        "Preview should contain the post content"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn ajax_request_to_unlock_returns_401_not_redirect_loop() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Simulate what happens after fetch auto-follows a 303 from an
    // auth-required route: an AJAX GET to /unlock without a session.
    // Previously this returned another 303 (infinite loop).
    // Now it should return 401 JSON.
    let resp = driver.ajax_get("/unlock").await;
    assert_eq!(resp.status(), 401);

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Session expired"),
        "Expected session expired message, got: {body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn default_avatar_returns_svg_directly() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (id, _secret) = driver.login_new_identity().await;

    // User has no avatar set — should get SVG directly (no redirect)
    let resp = driver.get(&format!("/profile/{id}/avatar")).await;
    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("Missing Content-Type")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "image/svg+xml");

    assert!(
        resp.headers().get(header::ETAG).is_some(),
        "Default avatar should have an ETag"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("<svg"),
        "Response body should contain SVG content"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn default_avatar_etag_returns_304() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (id, _secret) = driver.login_new_identity().await;

    let resp = driver.get(&format!("/profile/{id}/avatar")).await;
    assert_eq!(resp.status(), 200);
    let etag = resp
        .headers()
        .get(header::ETAG)
        .expect("Missing ETag")
        .to_str()
        .unwrap()
        .to_owned();

    // Second request with If-None-Match should return 304
    let resp = driver
        .get_if_none_match(&format!("/profile/{id}/avatar"), &etag)
        .await;
    assert_eq!(resp.status(), 304);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn avatar_by_id_has_24h_cache() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (id, _secret) = driver.login_new_identity().await;

    let resp = driver.get(&format!("/profile/{id}/avatar")).await;
    assert_eq!(resp.status(), 200);

    let cache_control = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .expect("Missing Cache-Control on avatar route")
        .to_str()
        .unwrap();
    assert_eq!(
        cache_control, "public, max-age=86400",
        "avatar route should cache for 24h"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn post_page_og_meta_resolves_rostra_mentions() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Create identity A and set display name "Alice" (via API)
    let resp = driver.api_get("/api/generate-id").await;
    let a_info: serde_json::Value = resp.json().await.unwrap();
    let a_id = a_info["rostra_id"].as_str().unwrap().to_string();
    let a_secret = a_info["rostra_id_secret"].as_str().unwrap().to_string();

    let resp = driver
        .api_post_json(
            &format!("/api/{a_id}/update-social-profile-managed"),
            Some(&a_secret),
            &json!({
                "display_name": "Alice",
                "bio": "Test identity",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    // Get current heads (profile update created events)
    let resp = driver.api_get(&format!("/api/{a_id}/heads")).await;
    let heads: serde_json::Value = resp.json().await.unwrap();
    let head = heads["heads"][0].as_str().unwrap();

    // Use identity A to publish a post mentioning itself (simplest: A's DB
    // already has A's profile, so the mention will resolve to "Alice")
    let resp = driver
        .api_post_json(
            &format!("/api/{a_id}/publish-social-post-managed"),
            Some(&a_secret),
            &json!({
                "parent_head_id": head,
                "content": format!("Hello <rostra:{a_id}>, welcome!"),
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let post: serde_json::Value = resp.json().await.unwrap();
    let event_id = post["event_id"].as_str().unwrap();

    // Log in as identity A (the author) via the web UI to view the post page
    // (each identity has its own DB, so only A can see A's post content)
    driver.login_with_secret(&a_id, &a_secret).await;

    let resp = driver.get(&format!("/post/{a_id}/{event_id}")).await;
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();

    // The og:description meta tag should contain @Alice, not the raw rostra: link
    assert!(
        body.contains("@Alice"),
        "OG meta should contain resolved @Alice mention, body:\n{body}"
    );
    assert!(
        !body.contains(&format!("rostra:{a_id}")),
        "OG meta should NOT contain raw rostra: link, body:\n{body}"
    );
}
