mod common;

use common::TestServer;

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
