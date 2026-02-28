mod common;

use common::TestServer;
use rostra_core::id::RostraIdSecretKey;

/// Helper: generate an identity via the API and return (rostra_id, secret).
async fn generate_identity(driver: &common::UiDriver) -> (String, String) {
    let resp = driver.api_get("/api/generate-id").await;
    let body: serde_json::Value = resp.json().await.unwrap();
    (
        body["rostra_id"].as_str().unwrap().to_string(),
        body["rostra_id_secret"].as_str().unwrap().to_string(),
    )
}

/// Helper: publish a post and return (event_id, heads).
async fn publish_post(
    driver: &common::UiDriver,
    rostra_id: &str,
    secret: &str,
    parent_head_id: Option<&str>,
    content: &str,
    reply_to: Option<&str>,
) -> (String, Vec<String>) {
    let mut body = serde_json::json!({
        "parent_head_id": parent_head_id,
        "content": content,
    });
    if let Some(rt) = reply_to {
        body["reply_to"] = serde_json::Value::String(rt.to_string());
    }
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &body,
        )
        .await;
    assert_eq!(resp.status(), 200, "Post should succeed");
    let result: serde_json::Value = resp.json().await.unwrap();
    let event_id = result["event_id"].as_str().unwrap().to_string();
    let heads: Vec<String> = result["heads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_string())
        .collect();
    (event_id, heads)
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn generate_id_returns_keypair() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["rostra_id"].as_str().is_some_and(|s| !s.is_empty()),
        "Expected non-empty rostra_id"
    );
    assert!(
        body["rostra_id_secret"]
            .as_str()
            .is_some_and(|s| s.split_whitespace().count() == 24),
        "Expected 24-word BIP39 mnemonic"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn missing_version_header_returns_400() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get_no_version("/api/generate-id").await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|s| s.contains("x-rostra-api-version")),
        "Error should mention the missing header, got: {body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn unsupported_version_returns_400() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get_with_version("/api/generate-id", "999").await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|s| s.contains("Unsupported")),
        "Error should mention unsupported version, got: {body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn heads_on_fresh_identity_returns_empty() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate an identity via the API
    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();

    // Query heads — should be empty for a fresh identity
    let resp = driver.api_get(&format!("/api/{rostra_id}/heads")).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["heads"], serde_json::json!([]));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_post_and_verify_heads() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate identity
    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    // Publish a post (first post, no parent_head_id needed)
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": null,
                "content": "Hello from the API!",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        200,
        "First post should succeed: {}",
        resp.text().await.unwrap()
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_post_full_flow() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // 1. Generate identity
    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    // 2. Verify heads are empty
    let resp = driver.api_get(&format!("/api/{rostra_id}/heads")).await;
    let heads_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(heads_body["heads"], serde_json::json!([]));

    // 3. Publish first post (parent_head_id = null for fresh identity)
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": null,
                "content": "First post!",
                "persona_tags": ["bot"],
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let post1: serde_json::Value = resp.json().await.unwrap();
    assert!(
        post1["event_id"].as_str().is_some_and(|s| !s.is_empty()),
        "Should return an event_id"
    );
    assert!(
        !post1["heads"].as_array().unwrap().is_empty(),
        "Should have at least one head after posting"
    );

    // 4. Verify heads are updated
    let resp = driver.api_get(&format!("/api/{rostra_id}/heads")).await;
    let heads_body: serde_json::Value = resp.json().await.unwrap();
    let heads = heads_body["heads"].as_array().unwrap();
    assert!(!heads.is_empty(), "Heads should not be empty after posting");

    // 5. Publish second post using a head from the first
    let head = heads[0].as_str().unwrap();
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": head,
                "content": "Second post!",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let post2: serde_json::Value = resp.json().await.unwrap();
    assert!(
        post2["event_id"].as_str().is_some_and(|s| !s.is_empty()),
        "Second post should return an event_id"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_twice_with_same_head_is_rejected() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate identity
    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    // First post (fresh identity, null parent)
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": null,
                "content": "Post one",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let post1: serde_json::Value = resp.json().await.unwrap();
    let head_after_first = post1["heads"][0].as_str().unwrap().to_string();

    // Second post using the head from the first
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": head_after_first,
                "content": "Post two",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    // Third post reusing the SAME head as the second post used — should fail
    // because the head changed after the second post succeeded
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": head_after_first,
                "content": "Post three (duplicate)",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        409,
        "Reusing a stale parent_head_id should return 409 Conflict"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|s| s.contains("not among current heads")),
        "Error should mention stale head, got: {body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_null_parent_rejected_when_heads_exist() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate identity and publish first post
    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": null,
                "content": "First!",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    // Try to publish with null parent_head_id again — heads exist now
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            Some(secret),
            &serde_json::json!({
                "parent_head_id": null,
                "content": "Second without parent",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        409,
        "Should reject null parent_head_id when heads exist"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_with_wrong_secret_returns_403() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate two identities
    let resp = driver.api_get("/api/generate-id").await;
    let id_info_1: serde_json::Value = resp.json().await.unwrap();
    let rostra_id_1 = id_info_1["rostra_id"].as_str().unwrap();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info_2: serde_json::Value = resp.json().await.unwrap();
    let secret_2 = id_info_2["rostra_id_secret"].as_str().unwrap();

    // Try to publish to identity 1 with identity 2's secret
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id_1}/publish-social-post-managed"),
            Some(secret_2),
            &serde_json::json!({
                "parent_head_id": null,
                "content": "Trying to impersonate",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        403,
        "Mismatched secret should return 403 Forbidden"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_without_secret_returns_401() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-managed"),
            None,
            &serde_json::json!({
                "parent_head_id": null,
                "content": "No secret",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        401,
        "Missing secret should return 401 Unauthorized"
    );
}

// -- Profile update tests --

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn update_profile_basic() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/update-social-profile-managed"),
            Some(secret),
            &serde_json::json!({
                "display_name": "Bot McBotface",
                "bio": "I am a test bot.",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        200,
        "Profile update should succeed: {}",
        resp.text().await.unwrap()
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn update_profile_returns_event_id_and_heads() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/update-social-profile-managed"),
            Some(secret),
            &serde_json::json!({
                "display_name": "Test Name",
                "bio": "Test bio",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["event_id"].as_str().is_some_and(|s| !s.is_empty()),
        "Should return an event_id"
    );
    assert!(
        !body["heads"].as_array().unwrap().is_empty(),
        "Should have heads after profile update"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn update_profile_with_avatar() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret = id_info["rostra_id_secret"].as_str().unwrap();

    // A few bytes of fake image data, base64-encoded
    let tiny_png_base64 = data_encoding::BASE64.encode(b"\x89PNG fake image data");

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/update-social-profile-managed"),
            Some(secret),
            &serde_json::json!({
                "display_name": "Avatar Bot",
                "bio": "I have a face!",
                "avatar": {
                    "mime_type": "image/png",
                    "base64": &tiny_png_base64,
                },
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        200,
        "Profile update with avatar should succeed: {}",
        resp.text().await.unwrap()
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn update_profile_wrong_secret_returns_403() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info_1: serde_json::Value = resp.json().await.unwrap();
    let rostra_id_1 = id_info_1["rostra_id"].as_str().unwrap();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info_2: serde_json::Value = resp.json().await.unwrap();
    let secret_2 = id_info_2["rostra_id_secret"].as_str().unwrap();

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id_1}/update-social-profile-managed"),
            Some(secret_2),
            &serde_json::json!({
                "display_name": "Impersonator",
                "bio": "Not really me",
            }),
        )
        .await;
    assert_eq!(resp.status(), 403);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn update_profile_without_secret_returns_401() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();

    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/update-social-profile-managed"),
            None,
            &serde_json::json!({
                "display_name": "No Auth",
                "bio": "Should fail",
            }),
        )
        .await;
    assert_eq!(resp.status(), 401);
}

// -- Notifications tests --

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn notifications_empty_for_fresh_identity() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (rostra_id, _secret) = generate_identity(&driver).await;

    let resp = driver
        .api_get(&format!("/api/{rostra_id}/notifications"))
        .await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["notifications"], serde_json::json!([]));
    assert!(body["next_cursor"].is_null());
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn notifications_response_structure() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Each identity has its own database in MultiClient, so cross-identity
    // notifications require p2p propagation (not available in tests).
    // This test validates the response structure and cursor fields.
    let (rostra_id, _secret) = generate_identity(&driver).await;

    let resp = driver
        .api_get(&format!("/api/{rostra_id}/notifications"))
        .await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["notifications"].is_array());
    assert!(
        body["next_cursor"].is_null(),
        "next_cursor should be null when there are no more pages"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn notifications_does_not_include_own_posts() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Identity A posts
    let (id_a, secret_a) = generate_identity(&driver).await;
    let (_event_id_a, _heads_a) =
        publish_post(&driver, &id_a, &secret_a, None, "My post", None).await;

    // A's notifications should NOT include A's own post
    let resp = driver.api_get(&format!("/api/{id_a}/notifications")).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let notifications = body["notifications"].as_array().unwrap();
    assert!(
        notifications.is_empty(),
        "Own posts should not appear in notifications"
    );
}

// -- Secretless publish tests --

/// Helper: call prepare, sign client-side, call publish, return (event_id,
/// heads).
async fn prepare_sign_publish(
    driver: &common::UiDriver,
    rostra_id: &str,
    secret: &RostraIdSecretKey,
    parent_head_id: Option<&str>,
    content: &str,
) -> (String, Vec<String>) {
    // 1. Prepare
    let body = serde_json::json!({
        "parent_head_id": parent_head_id,
        "content": content,
    });
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-prepare"),
            None,
            &body,
        )
        .await;
    assert_eq!(resp.status(), 200, "Prepare should succeed");
    let prepare: serde_json::Value = resp.json().await.unwrap();

    // 2. Sign client-side
    let event: rostra_core::event::Event =
        serde_json::from_value(prepare["event"].clone()).unwrap();
    let sig = event.sign_by(*secret);

    let content_hex = prepare["content"].as_str().unwrap();

    // 3. Publish
    let publish_body = serde_json::json!({
        "event": prepare["event"],
        "sig": serde_json::to_value(sig).unwrap(),
        "content": content_hex,
    });
    let resp = driver
        .api_post_json(&format!("/api/{rostra_id}/publish"), None, &publish_body)
        .await;
    assert_eq!(resp.status(), 200, "Publish should succeed");
    let result: serde_json::Value = resp.json().await.unwrap();
    let event_id = result["event_id"].as_str().unwrap().to_string();
    let heads: Vec<String> = result["heads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_string())
        .collect();
    (event_id, heads)
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn prepare_and_publish_round_trip() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate identity
    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret_str = id_info["rostra_id_secret"].as_str().unwrap();
    let secret: RostraIdSecretKey = secret_str.parse().unwrap();

    // First post (no parent head)
    let (event_id, heads) =
        prepare_sign_publish(&driver, rostra_id, &secret, None, "Hello secretless!").await;

    assert!(!event_id.is_empty(), "Should return an event_id");
    assert!(!heads.is_empty(), "Should have heads after posting");

    // Second post using a head from the first
    let head = &heads[0];
    let (event_id_2, heads_2) =
        prepare_sign_publish(&driver, rostra_id, &secret, Some(head), "Second post!").await;

    assert!(!event_id_2.is_empty());
    assert!(!heads_2.is_empty());
    assert_ne!(
        event_id, event_id_2,
        "Different posts should have different event IDs"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_with_wrong_author_returns_403() {
    let server = TestServer::start().await;
    let driver = server.driver();

    // Generate two identities
    let resp = driver.api_get("/api/generate-id").await;
    let id_info_1: serde_json::Value = resp.json().await.unwrap();
    let rostra_id_1 = id_info_1["rostra_id"].as_str().unwrap();
    let secret_1: RostraIdSecretKey = id_info_1["rostra_id_secret"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info_2: serde_json::Value = resp.json().await.unwrap();
    let rostra_id_2 = id_info_2["rostra_id"].as_str().unwrap();

    // Prepare an event for identity 1
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id_1}/publish-social-post-prepare"),
            None,
            &serde_json::json!({
                "parent_head_id": null,
                "content": "test",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let prepare: serde_json::Value = resp.json().await.unwrap();

    let event: rostra_core::event::Event =
        serde_json::from_value(prepare["event"].clone()).unwrap();
    let sig = event.sign_by(secret_1);

    // Try to publish to identity 2's URL (author mismatch)
    let publish_body = serde_json::json!({
        "event": prepare["event"],
        "sig": serde_json::to_value(sig).unwrap(),
        "content": prepare["content"],
    });
    let resp = driver
        .api_post_json(&format!("/api/{rostra_id_2}/publish"), None, &publish_body)
        .await;
    assert_eq!(
        resp.status(),
        403,
        "Publishing to wrong identity should return 403"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_with_bad_signature_returns_400() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();

    // Prepare an event
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-prepare"),
            None,
            &serde_json::json!({
                "parent_head_id": null,
                "content": "test",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let prepare: serde_json::Value = resp.json().await.unwrap();

    // Use a garbage signature (64 zero bytes, hex-encoded)
    let bad_sig = "0".repeat(128);

    let publish_body = serde_json::json!({
        "event": prepare["event"],
        "sig": bad_sig,
        "content": prepare["content"],
    });
    let resp = driver
        .api_post_json(&format!("/api/{rostra_id}/publish"), None, &publish_body)
        .await;
    assert_eq!(resp.status(), 400, "Bad signature should return 400");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn publish_with_mismatched_content_returns_400() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();
    let secret: RostraIdSecretKey = id_info["rostra_id_secret"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Prepare an event
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-prepare"),
            None,
            &serde_json::json!({
                "parent_head_id": null,
                "content": "test",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let prepare: serde_json::Value = resp.json().await.unwrap();

    let event: rostra_core::event::Event =
        serde_json::from_value(prepare["event"].clone()).unwrap();
    let sig = event.sign_by(secret);

    // Send with tampered content (different hex bytes)
    let publish_body = serde_json::json!({
        "event": prepare["event"],
        "sig": serde_json::to_value(sig).unwrap(),
        "content": "deadbeef",
    });
    let resp = driver
        .api_post_json(&format!("/api/{rostra_id}/publish"), None, &publish_body)
        .await;
    assert_eq!(resp.status(), 400, "Mismatched content should return 400");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn prepare_does_not_require_secret() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.api_get("/api/generate-id").await;
    let id_info: serde_json::Value = resp.json().await.unwrap();
    let rostra_id = id_info["rostra_id"].as_str().unwrap();

    // Prepare without any secret header — should succeed
    let resp = driver
        .api_post_json(
            &format!("/api/{rostra_id}/publish-social-post-prepare"),
            None,
            &serde_json::json!({
                "parent_head_id": null,
                "content": "No secret needed!",
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        200,
        "Prepare should not require a secret header"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["event"].is_object(), "Should return event object");
    assert!(
        body["content"].as_str().is_some_and(|s| !s.is_empty()),
        "Should return hex-encoded content"
    );
}
