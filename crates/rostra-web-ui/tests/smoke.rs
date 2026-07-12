mod common;

use common::TestServer;
use reqwest::header;
use rostra_core::id::RostraIdSecretKey;
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

    let resp = driver.get("/following").await;
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

    let resp = driver.get("/following").await;
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

fn csrf_token(page: &str) -> &str {
    let marker = "name=\"csrf_token\" value=\"";
    let remainder = page
        .split_once(marker)
        .expect("page should contain a CSRF token")
        .1;
    remainder
        .split_once('"')
        .expect("CSRF token value should be quoted")
        .0
}

fn assert_identity_action_controls(page: &str, disabled: bool) {
    assert!(!page.contains("Identity &amp; recovery"));
    assert!(page.contains("m-identityRecovery__copyButton u-button"));
    assert!(page.contains("m-identityRecovery__copyButtonIcon u-buttonIcon"));
    assert!(page.contains("aria-label=\"Copy RostraId\""));
    assert!(page.contains(">RostraId</button>"));
    assert!(page.contains("m-identityRecovery__revealButton u-button"));
    assert!(page.contains("m-identityRecovery__revealButtonIcon u-buttonIcon"));
    assert!(page.contains(">Reveal</button>"));
    let reveal_button = page
        .split_once("m-identityRecovery__revealButton u-button")
        .expect("identity page should contain the reveal button")
        .1
        .split_once("</button>")
        .expect("reveal button should have a closing tag")
        .0;
    assert_eq!(reveal_button.contains("disabled"), disabled);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn identity_actions_and_recovery_reveal_are_protected_and_session_scoped() {
    let server = TestServer::start().await;
    let rw = server.driver();
    let (id, secret) = rw.login_new_identity().await;
    let phrase = secret.to_string();

    let resp = rw.get("/settings/identity").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store, private"
    );
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        resp.headers().get("content-security-policy").unwrap(),
        "frame-ancestors 'none'"
    );
    let page = resp.text().await.unwrap();
    assert!(!page.contains(&phrase));
    assert_identity_action_controls(&page, false);
    let csrf = csrf_token(&page);

    let resp = rw
        .same_origin_post_form_accept_br(
            "/settings/identity/recovery-phrase",
            &[("csrf_token", csrf)],
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store, private"
    );
    assert_eq!(
        resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "identity"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains(&phrase));
    assert!(body.contains("readonly"));
    assert!(body.contains("Copy recovery phrase"));

    let resp = rw
        .same_origin_post_form(
            "/settings/identity/recovery-phrase",
            &[("csrf_token", "invalid")],
        )
        .await;
    assert_eq!(resp.status(), 403);

    let resp = rw
        .cross_site_post_form(
            "/settings/identity/recovery-phrase",
            &[("csrf_token", csrf)],
        )
        .await;
    assert_eq!(resp.status(), 403);

    let ro = server.driver();
    ro.login_readonly(id).await;
    let page = ro.get("/settings/identity").await.text().await.unwrap();
    assert!(page.contains("This session does not hold the recovery phrase"));
    assert!(!page.contains(&phrase));
    assert_identity_action_controls(&page, true);
    let ro_csrf = csrf_token(&page);
    let resp = ro
        .same_origin_post_form(
            "/settings/identity/recovery-phrase",
            &[("csrf_token", ro_csrf)],
        )
        .await;
    assert_eq!(resp.status(), 403);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn recovery_phrase_has_no_get_or_unauthenticated_access() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.get("/settings/identity/recovery-phrase").await;
    assert_eq!(resp.status(), 405);

    let resp = driver
        .same_origin_post_form(
            "/settings/identity/recovery-phrase",
            &[("csrf_token", "invalid")],
        )
        .await;
    assert_eq!(resp.status(), 303);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("/unlock")
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn account_creation_generates_selectable_24_word_phrase_by_post() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.get("/unlock/random").await;
    assert_eq!(resp.status(), 404);

    let resp = driver
        .same_origin_post_form_accept_br("/unlock/generate", &[("redirect", "/following")])
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store, private"
    );
    assert_eq!(
        resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "identity"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("I saved this recovery phrase"));
    assert!(body.contains("name=\"redirect\" value=\"/following\""));
    assert!(body.contains("readonly"));
    assert!(!body.contains("12 words"));

    let phrase = body
        .split_once("id=\"recovery-phrase\"")
        .unwrap()
        .1
        .split_once('>')
        .unwrap()
        .1
        .split_once("</textarea>")
        .unwrap()
        .0;
    assert_eq!(phrase.split_whitespace().count(), 24);

    for invalid in [
        "//attacker.example",
        r#"/\attacker.example"#,
        "https://attacker.example/",
        "/bad\r\nlocation",
    ] {
        let resp = driver
            .post_form("/unlock/generate", &[("redirect", invalid)])
            .await;
        let body = resp.text().await.unwrap();
        assert!(
            !body.contains("attacker.example") && !body.contains("bad"),
            "unsafe redirect was reflected: {invalid:?}"
        );
    }

    let secret = RostraIdSecretKey::generate();
    let id = secret.id().to_string();
    let phrase = secret.to_string();
    let resp = driver
        .post_form(
            "/unlock",
            &[
                ("username", &id),
                ("password", &phrase),
                ("redirect", r#"/\attacker.example"#),
            ],
        )
        .await;
    assert_eq!(resp.status(), 303);
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/");

    let resp = driver
        .post_form(
            "/unlock",
            &[
                ("username", &id),
                ("password", &phrase),
                ("redirect", "/path?query=value"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 303);
    assert_eq!(
        resp.headers().get(header::LOCATION).unwrap(),
        "/path?query=value"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn credential_export_requires_https_off_loopback() {
    let server = TestServer::start_non_loopback_http().await;
    let driver = server.driver();

    let resp = driver.post_form("/unlock/generate", &[]).await;
    assert_eq!(resp.status(), 403);

    let secret = RostraIdSecretKey::generate();
    let id = secret.id().to_string();
    let phrase = secret.to_string();
    let resp = driver
        .post_form("/unlock", &[("username", &id), ("password", &phrase)])
        .await;
    assert_eq!(resp.status(), 303);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Secure"));
    let cookie_pair = cookie.split(';').next().unwrap();

    let page = driver
        .get_with_cookie("/settings/identity", cookie_pair)
        .await
        .text()
        .await
        .unwrap();
    assert!(page.contains("Recovery phrase reveal is disabled"));
    let csrf = csrf_token(&page);
    let resp = driver
        .same_origin_post_form_with_cookie(
            "/settings/identity/recovery-phrase",
            cookie_pair,
            &[("csrf_token", csrf)],
        )
        .await;
    assert_eq!(resp.status(), 403);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn public_http_origin_overrides_loopback_bind_security() {
    let server = TestServer::start_public_http_origin().await;
    let driver = server.driver();

    let resp = driver.post_form("/unlock/generate", &[]).await;
    assert_eq!(resp.status(), 403);

    let secret = RostraIdSecretKey::generate();
    let id = secret.id().to_string();
    let phrase = secret.to_string();
    let resp = driver
        .post_form("/unlock", &[("username", &id), ("password", &phrase)])
        .await;
    assert_eq!(resp.status(), 303);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("Secure"));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn loopback_https_origin_uses_secure_cookie() {
    let server = TestServer::start_loopback_https_origin().await;
    let driver = server.driver();
    let secret = RostraIdSecretKey::generate();
    let id = secret.id().to_string();
    let phrase = secret.to_string();

    let resp = driver
        .post_form("/unlock", &[("username", &id), ("password", &phrase)])
        .await;
    assert_eq!(resp.status(), 303);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("Secure"));

    let resp = driver.post_form("/unlock/generate", &[]).await;
    assert_eq!(resp.status(), 200);
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
