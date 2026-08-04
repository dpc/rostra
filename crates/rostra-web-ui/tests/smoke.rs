mod common;

use common::TestServer;
use reqwest::header;
use rostra_core::event::{Event, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, ShortEventId};
use scraper::{ElementRef, Html, Selector};
use serde_json::json;

fn owning_form<'a>(document: &'a Html, control: ElementRef<'a>) -> Option<ElementRef<'a>> {
    if let Some(form_id) = control.value().attr("form") {
        let any_element = Selector::parse("*").unwrap();
        return document
            .select(&any_element)
            .find(|element| element.value().id() == Some(form_id))
            .filter(|element| element.value().name() == "form");
    }

    control
        .ancestors()
        .filter_map(ElementRef::wrap)
        .find(|element| element.value().name() == "form")
}

fn assert_link_precedes(document: &Html, first: &str, second: &str) {
    let links = Selector::parse("a[href]").unwrap();
    let hrefs: Vec<_> = document
        .select(&links)
        .filter_map(|link| link.value().attr("href"))
        .collect();
    let first_index = hrefs
        .iter()
        .position(|href| *href == first)
        .unwrap_or_else(|| panic!("missing link to {first}"));
    let second_index = hrefs
        .iter()
        .position(|href| *href == second)
        .unwrap_or_else(|| panic!("missing link to {second}"));
    assert!(
        first_index < second_index,
        "expected {first} to precede {second}, found {hrefs:?}"
    );
}

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

    let resp = driver.get("/").await;
    assert_eq!(resp.status(), 307);
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/home");

    let resp = driver.get("/home").await;
    assert_eq!(resp.status(), 307);
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/following");

    let resp = driver.get("/following").await;
    assert_eq!(resp.status(), 200);

    let document = Html::parse_document(&resp.text().await.unwrap());
    assert_link_precedes(&document, "/following", "/news");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn explicit_news_url_remains_available() {
    let server = TestServer::start().await;
    let driver = server.driver();

    driver.login_new_identity().await;

    let resp = driver.get("/news").await;
    assert_eq!(resp.status(), 200);

    let document = Html::parse_document(&resp.text().await.unwrap());
    assert_link_precedes(&document, "/following", "/news");
    let active_news = Selector::parse(r#"a[href="/news"][aria-current="page"]"#).unwrap();
    assert_eq!(
        document.select(&active_news).count(),
        1,
        "the explicit News URL should keep News selected"
    );

    let resp = driver.get("/shoutbox").await;
    assert_eq!(resp.status(), 200);
    let document = Html::parse_document(&resp.text().await.unwrap());
    assert_link_precedes(&document, "/following", "/news");

    let resp = driver.get("/sitemap.xml").await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    let following = body
        .find("/following</loc>")
        .expect("sitemap should include Following");
    let news = body
        .find("/news</loc>")
        .expect("sitemap should include News");
    assert!(
        following < news,
        "sitemap should list Following before News: {body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn post_urls_use_short_author_ids_and_redirect_long_urls() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (author, _) = driver.login_new_identity().await;

    let response = driver
        .ajax_post_form("/post", &[("content", "A canonical post URL")])
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let post_url = document
        .select(&Selector::parse("[data-href]").unwrap())
        .filter_map(|element| element.value().attr("data-href"))
        .find(|href| href.starts_with("/post/"))
        .expect("new post should include a post URL")
        .to_owned();
    let event_id = post_url
        .rsplit('/')
        .next()
        .expect("post URL has event ID")
        .to_owned();

    let expected_post_url = format!("/post/{}/{event_id}", author.to_short());
    assert_eq!(post_url, expected_post_url);

    let response = driver.get(&expected_post_url).await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    let document = Html::parse_document(&body);
    let canonical_url = document
        .select(&Selector::parse(r#"link[rel="canonical"]"#).unwrap())
        .next()
        .and_then(|element| element.value().attr("href"))
        .expect("post page should include a canonical URL");
    assert!(
        canonical_url.ends_with(&expected_post_url),
        "canonical URL should use the short author ID: {canonical_url}"
    );
    assert!(
        body.contains(&format!(r#""url":"{canonical_url}""#)),
        "JSON-LD should use the canonical URL: {body}"
    );

    for legacy_author in [
        author.to_string(),
        author.to_unprefixed_z32_string(),
        author.to_bech32_string(),
    ] {
        let response = driver
            .get(&format!("/post/{legacy_author}/{event_id}?raw=true"))
            .await;
        assert_eq!(response.status(), 308);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            &format!("{expected_post_url}?raw=true")
        );
    }

    let response = driver
        .get(&format!("/profile/{}/atom.xml", author.to_short()))
        .await;
    assert_eq!(response.status(), 200);
    assert!(
        response.text().await.unwrap().contains(&expected_post_url),
        "Atom feed should use the canonical short post URL"
    );

    let unknown_author = RostraIdSecretKey::generate().id().to_short();
    let response = driver
        .get(&format!("/post/{unknown_author}/{event_id}"))
        .await;
    assert_eq!(response.status(), 404);

    let missing_event_id = ShortEventId::from_bytes([24; 16]);
    let response = driver
        .get(&format!("/post/{author}/{missing_event_id}"))
        .await;
    assert_eq!(response.status(), 404);

    let event_id = event_id
        .parse::<ShortEventId>()
        .expect("post URL has a valid short event ID");
    let full_event_id = server
        .client(author)
        .await
        .db()
        .get_event(event_id)
        .await
        .expect("published post retains its envelope")
        .signed
        .compute_id();
    let response = driver
        .get(&format!(
            "/post/{}/{full_event_id}?raw=true&source=legacy",
            author.to_short()
        ))
        .await;
    assert_eq!(response.status(), 308);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!(
            "/post/{}/{event_id}?raw=true&source=legacy",
            author.to_short()
        )
    );

    let mut forged_event_id: [u8; 32] = full_event_id.into();
    forged_event_id[31] ^= 1;
    let forged_event_id = EventId::from_bytes(forged_event_id);
    let response = driver
        .get(&format!("/post/{}/{forged_event_id}", author.to_short()))
        .await;
    assert_eq!(response.status(), 404);

    let edit_query = format!("post_thread_id={event_id}&post_target_id=post-target");
    let response = driver
        .get(&format!("/post/{author}/{full_event_id}/edit?{edit_query}"))
        .await;
    assert_eq!(response.status(), 308);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!("{expected_post_url}/edit?{edit_query}")
    );

    let response = driver
        .get(&format!(
            "/post/{author}/{full_event_id}/edit_cancel?{edit_query}"
        ))
        .await;
    assert_eq!(response.status(), 308);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!("{expected_post_url}/edit_cancel?{edit_query}")
    );

    let event_id_string = event_id.to_string();
    let response = driver
        .post_form(
            &format!("/post/{author}/{full_event_id}/edit"),
            &[
                ("content", "Updated canonical post URL"),
                ("post_thread_id", &event_id_string),
                ("post_target_id", "post-target"),
            ],
        )
        .await;
    assert_eq!(response.status(), 200);

    let response = driver
        .post_form(&format!("/post/{author}/{full_event_id}/delete"), &[])
        .await;
    assert_eq!(response.status(), 200);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn post_url_rejects_another_author_retained_envelope_without_content() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (requested_author, _) = driver.login_new_identity().await;

    let actual_author_secret = RostraIdSecretKey::generate();
    let event = Event::builder_raw_content()
        .author(actual_author_secret.id())
        .kind(EventKind::RAW)
        .build();
    let event = VerifiedEvent::verify_received_as_is(event.signed_by(actual_author_secret))
        .expect("fixture event verifies");
    server
        .client(requested_author)
        .await
        .db()
        .try_process_event(&event)
        .await
        .expect("store retained envelope without content");

    let response = driver
        .get(&format!(
            "/post/{}/{}",
            requested_author.to_short(),
            event.event_id.to_short()
        ))
        .await;

    assert_eq!(response.status(), 404);

    let response = driver
        .get(&format!(
            "/post/{}/{}",
            requested_author.to_short(),
            event.event_id
        ))
        .await;

    assert_eq!(response.status(), 404);

    let response = driver
        .get(&format!(
            "/media/{}/{}",
            requested_author.to_short(),
            event.event_id.to_short()
        ))
        .await;

    assert_eq!(response.status(), 404);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn full_event_resource_urls_validate_and_canonicalize() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (author, author_secret) = driver.login_new_identity().await;
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::RAW)
        .build();
    let event = VerifiedEvent::verify_received_as_is(event.signed_by(author_secret))
        .expect("fixture event verifies");
    server
        .client(author)
        .await
        .db()
        .try_process_event(&event)
        .await
        .expect("store retained envelope without content");

    let full_event_id = event.event_id;
    let event_id = full_event_id.to_short();
    let identity_cases = [
        (
            format!("/profile/{author}?source=legacy"),
            format!("/profile/{}?source=legacy", author.to_short()),
        ),
        (
            format!("/profile/{author}/atom.xml?source=legacy"),
            format!("/profile/{}/atom.xml?source=legacy", author.to_short()),
        ),
        (
            format!("/profile/{author}/follow?following=true&source=legacy"),
            format!(
                "/profile/{}/follow?following=true&source=legacy",
                author.to_short()
            ),
        ),
        (
            format!("/profile/{author}/avatar?v=legacy"),
            format!("/profile/{}/avatar?v=legacy", author.to_short()),
        ),
        (
            format!("/media/{author}/list?target=%23content&source=legacy"),
            format!(
                "/media/{}/list?target=%23content&source=legacy",
                author.to_short()
            ),
        ),
    ];
    for (legacy_url, canonical_url) in identity_cases {
        let response = driver.get(&legacy_url).await;
        assert_eq!(response.status(), 308, "legacy URL: {legacy_url}");
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            &canonical_url
        );
    }

    let cases = [
        (
            format!("/media/{author}/{full_event_id}?download=1"),
            format!("/media/{}/{event_id}?download=1", author.to_short()),
        ),
        (
            format!("/settings/events/content/{full_event_id}?pretty=1"),
            format!("/settings/events/content/{event_id}?pretty=1"),
        ),
        (
            format!("/post/{full_event_id}/{author}/{full_event_id}/fetch?source=legacy"),
            format!(
                "/post/{event_id}/{}/{event_id}/fetch?source=legacy",
                author.to_short()
            ),
        ),
        (
            format!("/replies/{full_event_id}/{full_event_id}?source=legacy"),
            format!("/replies/{event_id}/{event_id}?source=legacy"),
        ),
    ];

    for (legacy_url, canonical_url) in cases {
        let response = driver.get(&legacy_url).await;
        assert_eq!(response.status(), 308, "legacy URL: {legacy_url}");
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            &canonical_url
        );
    }

    let response = driver
        .head(&format!("/post/{author}/{full_event_id}?source=legacy"))
        .await;
    assert_eq!(response.status(), 308);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!("/post/{}/{event_id}?source=legacy", author.to_short())
    );

    let response = driver
        .head(&format!(
            "/post/{full_event_id}/{author}/{full_event_id}/fetch?source=legacy"
        ))
        .await;
    assert_eq!(response.status(), 308);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!(
            "/post/{event_id}/{}/{event_id}/fetch?source=legacy",
            author.to_short()
        )
    );

    let response = driver
        .post_form(
            &format!("/post/{full_event_id}/{author}/{full_event_id}/fetch"),
            &[],
        )
        .await;
    assert_eq!(response.status(), 200);

    let missing_event_id = ShortEventId::from_bytes([25; 16]);
    let response = driver
        .post_form(
            &format!(
                "/post/{missing_event_id}/{}/{missing_event_id}/fetch",
                author.to_short()
            ),
            &[],
        )
        .await;
    assert_eq!(response.status(), 200);

    let mut forged_event_id: [u8; 32] = full_event_id.into();
    forged_event_id[31] ^= 1;
    let forged_event_id = EventId::from_bytes(forged_event_id);
    for url in [
        format!("/media/{}/{forged_event_id}", author.to_short()),
        format!("/settings/events/content/{forged_event_id}"),
        format!("/replies/{event_id}/{forged_event_id}"),
    ] {
        let response = driver.get(&url).await;
        assert_eq!(response.status(), 404, "forged URL: {url}");
    }
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
async fn identity_recovery_phrase_is_masked_protected_and_session_scoped() {
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
    assert_eq!(resp.headers().get(header::PRAGMA).unwrap(), "no-cache");
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        resp.headers().get("content-security-policy").unwrap(),
        "frame-ancestors 'none'"
    );
    assert_eq!(
        resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "identity"
    );
    let page = resp.text().await.unwrap();
    assert!(page.contains(&phrase));
    assert!(page.contains("type=\"password\""));
    assert!(page.contains("readonly"));
    assert!(page.contains("aria-label=\"Copy recovery phrase\""));
    assert!(page.contains(">Copy</button>"));
    assert!(page.contains("role=\"status\" aria-live=\"polite\""));
    assert!(!page.contains("<dialog"));
    assert!(!page.contains(">Reveal"));

    let ro = server.driver();
    ro.login_readonly(id).await;
    let page = ro.get("/settings/identity").await.text().await.unwrap();
    assert!(page.contains("This session does not hold the recovery phrase"));
    assert!(!page.contains(&phrase));
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
    assert_eq!(resp.headers().get(header::PRAGMA).unwrap(), "no-cache");
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        resp.headers().get("content-security-policy").unwrap(),
        "frame-ancestors 'none'"
    );
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("<!DOCTYPE html>"));
    assert!(body.contains("<html lang=\"en\"><head>"));
    assert!(body.contains("<title>Save recovery phrase</title>"));
    assert!(body.contains("<h1>Create account</h1>"));
    assert!(!body.contains("I saved this recovery phrase"));
    assert!(body.contains(">Continue with new account</button>"));
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
    let generated_id = body
        .split_once("name=\"username\" value=\"")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let resp = driver
        .post_form(
            "/unlock",
            &[
                ("username", generated_id),
                ("password", phrase),
                ("redirect", "/following"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 303);
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/following");

    let resp = driver
        .ajax_post_form("/unlock/generate", &[("redirect", "/following")])
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store, private"
    );
    let body = resp.text().await.unwrap();
    assert!(!body.contains("<!DOCTYPE html>"));
    assert!(body.contains("<form id=\"create-account-form\""));
    assert!(body.contains("name=\"redirect\" value=\"/following\""));

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
async fn create_account_control_is_contained_by_login_card_and_targets_generate_form() {
    let server = TestServer::start().await;
    let body = server
        .driver()
        .get("/unlock?redirect=%2Ffollowing")
        .await
        .text()
        .await
        .unwrap();
    let document = Html::parse_document(&body);

    let login_form_selector = Selector::parse("form.o-unlockScreen__form").unwrap();
    let generate_form_selector = Selector::parse("form#generate-account-form").unwrap();
    let generate_id_selector = Selector::parse("#generate-account-form").unwrap();
    let generate_button_selector = Selector::parse("button[form='generate-account-form']").unwrap();
    let controls_selector = Selector::parse("input, textarea, select, button").unwrap();
    let username_selector = Selector::parse("[name='username']").unwrap();
    let password_selector = Selector::parse("[name='password']").unwrap();

    let login_forms = document.select(&login_form_selector).collect::<Vec<_>>();
    assert_eq!(login_forms.len(), 1, "login form must be unique");
    let login_form = login_forms[0];
    assert_eq!(
        login_form.select(&generate_button_selector).count(),
        1,
        "Create Account control must be uniquely contained by the login card"
    );
    assert!(
        login_form.select(&generate_form_selector).next().is_none(),
        "generation form must not be nested in the login form"
    );
    for credential_selector in [&username_selector, &password_selector] {
        let credentials = document.select(credential_selector).collect::<Vec<_>>();
        assert_eq!(credentials.len(), 1, "login credential must be unique");
        assert_eq!(
            owning_form(&document, credentials[0]),
            Some(login_form),
            "login credential must remain owned by the login form"
        );
    }

    let generate_id_matches = document.select(&generate_id_selector).collect::<Vec<_>>();
    assert_eq!(
        generate_id_matches.len(),
        1,
        "generation form ID must be unique document-wide"
    );
    let generate_forms = document.select(&generate_form_selector).collect::<Vec<_>>();
    assert_eq!(generate_forms.len(), 1, "generation form ID must be unique");
    let generate_form = generate_forms[0];
    assert_eq!(generate_id_matches[0], generate_form);
    assert_eq!(
        generate_form.value().attr("action"),
        Some("/unlock/generate")
    );
    assert_eq!(generate_form.value().attr("method"), Some("post"));
    let generation_controls = document
        .select(&controls_selector)
        .filter(|control| owning_form(&document, *control) == Some(generate_form))
        .collect::<Vec<_>>();
    assert_eq!(
        generation_controls.len(),
        2,
        "generation form owns only its submitter and redirect"
    );
    assert_eq!(
        generation_controls
            .iter()
            .filter_map(|control| control.value().attr("name"))
            .collect::<Vec<_>>(),
        ["redirect"]
    );
    assert_eq!(
        generation_controls
            .iter()
            .filter(|control| control.value().name() == "button")
            .count(),
        1,
        "generation form must own exactly one submitter"
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
    assert!(page.contains("Recovery phrase display is disabled"));
    assert!(!page.contains(&phrase));
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
    let resp = driver
        .get(&format!("/profile/{}/avatar", id.to_short()))
        .await;
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

    let resp = driver
        .get(&format!("/profile/{}/avatar", id.to_short()))
        .await;
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
        .get_if_none_match(&format!("/profile/{}/avatar", id.to_short()), &etag)
        .await;
    assert_eq!(resp.status(), 304);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn avatar_by_id_has_24h_cache() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (id, _secret) = driver.login_new_identity().await;

    let resp = driver
        .get(&format!("/profile/{}/avatar", id.to_short()))
        .await;
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
    let mut head = heads["heads"][0].as_str().unwrap().to_owned();

    // Log in as identity A (the author) via the web UI to view the post page
    // (each identity has its own DB, so only A can see A's post content)
    driver.login_with_secret(&a_id, &a_secret).await;

    let author = a_id.parse::<RostraId>().expect("API returned RostraId");
    let social_title = "Alice's post on Rostra";

    for (form, mention_id) in [
        ("full", a_id.clone()),
        ("short", author.to_short().to_string()),
    ] {
        let resp = driver
            .api_post_json(
                &format!("/api/{a_id}/publish-social-post-managed"),
                Some(&a_secret),
                &json!({
                    "parent_head_id": head,
                    "content": format!("Hello <rostra:{mention_id}>, welcome!"),
                }),
            )
            .await;
        assert_eq!(resp.status(), 200);
        let post: serde_json::Value = resp.json().await.unwrap();
        let event_id = post["event_id"].as_str().unwrap().to_owned();
        head = event_id.clone();

        let resp = driver
            .get(&format!("/post/{}/{event_id}", author.to_short()))
            .await;
        assert_eq!(resp.status(), 200);

        let body = resp.text().await.unwrap();
        let document = Html::parse_document(&body);

        assert_eq!(
            document
                .select(&Selector::parse("title").unwrap())
                .next()
                .map(|title| title.text().collect::<String>()),
            Some(social_title.to_owned()),
            "{form} mention should use the shared social title"
        );
        for selector in [
            r#"meta[property="og:title"]"#,
            r#"meta[name="twitter:title"]"#,
        ] {
            assert_eq!(
                document
                    .select(&Selector::parse(selector).unwrap())
                    .next()
                    .and_then(|meta| meta.value().attr("content")),
                Some(social_title),
                "{form} mention {selector} should use the shared social title"
            );
        }

        assert!(
            body.contains("@Alice"),
            "{form} mention should normalize in post social metadata, body:\n{body}"
        );
        assert!(
            !body.contains(&format!("rostra:{mention_id}")),
            "{form} mention should not retain its raw Rostra link, body:\n{body}"
        );
        assert!(
            body.contains(&format!("href=\"/profile/{}\"", author.to_short())),
            "{form} mention should resolve to the canonical short profile route, body:\n{body}"
        );
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn post_page_keeps_unknown_short_rostra_mentions_unresolved() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (author, secret) = driver.login_new_identity().await;
    let unknown_id = RostraIdSecretKey::generate().id();
    let resp = driver.api_get(&format!("/api/{author}/heads")).await;
    let heads: serde_json::Value = resp.json().await.unwrap();
    let head = heads["heads"][0].as_str().unwrap();
    let unknown_short_id = unknown_id.to_short();

    let resp = driver
        .api_post_json(
            &format!("/api/{author}/publish-social-post-managed"),
            Some(&secret.to_string()),
            &json!({
                "parent_head_id": head,
                "content": format!("Hello <rostra:{unknown_short_id}>"),
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let post: serde_json::Value = resp.json().await.unwrap();
    let event_id = post["event_id"].as_str().unwrap();

    let resp = driver
        .get(&format!("/post/{}/{event_id}", author.to_short()))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    assert!(
        body.contains(&format!("rostra:{unknown_short_id}")),
        "unknown short mentions should retain safe fallback text, body:\n{body}"
    );
    assert!(
        !body.contains(&format!("href=\"/profile/{unknown_id}\"")),
        "unknown short mentions must not select a full identity, body:\n{body}"
    );
    assert!(
        body.contains(&format!("href=\"rostra:{unknown_short_id}\"")),
        "unknown short mentions should preserve the established sanitized fallback, body:\n{body}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn profile_search_emits_short_ids_for_shared_mention_autocomplete() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (author, secret) = driver.login_new_identity().await;

    let resp = driver.api_get(&format!("/api/{author}/heads")).await;
    let heads: serde_json::Value = resp.json().await.unwrap();
    let head = heads["heads"][0].as_str().unwrap();
    let resp = driver
        .api_post_json(
            &format!("/api/{author}/publish-social-post-managed"),
            Some(&secret.to_string()),
            &json!({
                "parent_head_id": head,
                "content": "index this identity",
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let resp = driver.get("/search/profiles?q=rs").await;
    assert_eq!(resp.status(), 200);
    let results: serde_json::Value = resp.json().await.unwrap();
    let author_short = author.to_short().to_string();
    let author_full = author.to_string();
    assert_eq!(
        results[0]["rostra_id_reference"].as_str(),
        Some(author_short.as_str())
    );
    assert_ne!(
        results[0]["rostra_id_reference"].as_str(),
        Some(author_full.as_str())
    );

    let unretained_followee = RostraIdSecretKey::generate().id();
    let unretained_followee_full = unretained_followee.to_string();
    let resp = driver
        .post_form("/followee", &[("rostra_id", &unretained_followee_full)])
        .await;
    assert_eq!(resp.status(), 200);

    let resp = driver
        .get(&format!("/search/profiles?q={unretained_followee_full}"))
        .await;
    assert_eq!(resp.status(), 200);
    let results: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        results[0]["rostra_id_reference"].as_str(),
        Some(unretained_followee_full.as_str()),
        "a followed identity without retained authored events must retain a full mention ID"
    );

    for path in ["/following", "/shoutbox"] {
        let resp = driver.get(path).await;
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("x-data=\"textAutocomplete\""),
            "{path} should emit shared autocomplete markup, body:\n{body}"
        );
    }
}

#[test]
fn all_mention_composers_use_the_shared_short_id_selection_contract() {
    let app = include_str!("../assets/app.js");
    assert!(
        app.contains("insertText = `<rostra:${result.rostra_id_reference}>`;"),
        "the shared selection handler must insert the server-provided identity ID"
    );

    let new_post = include_str!("../src/routes/new_post.rs");
    let bindings = new_post
        .match_indices("x-data=\"textAutocomplete\"")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(
        bindings.len(),
        3,
        "new post has three autocomplete composers"
    );
    for (binding, marker, composer) in [
        (
            bindings[0],
            "@let textarea_id = format!(\"inline-reply-content",
            "inline reply",
        ),
        (
            bindings[1],
            "placeholder=\"Discussion text (optional)\"",
            "news post",
        ),
        (bindings[2], "\"What's on your mind?\"", "new post"),
    ] {
        let marker = new_post.find(marker).expect("composer marker");
        assert!(
            binding < marker,
            "{composer} autocomplete binding must wrap its textarea"
        );
    }

    for (surface, markup, marker) in [
        (
            "post edit",
            include_str!("../src/routes/post.rs"),
            "placeholder=\"Edit post...\"",
        ),
        (
            "shoutbox",
            include_str!("../src/routes/shoutbox.rs"),
            "placeholder=\"Shout something...\"",
        ),
    ] {
        let bindings = markup
            .match_indices("x-data=\"textAutocomplete\"")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(bindings.len(), 1, "{surface} has one autocomplete composer");
        let marker = markup.find(marker).expect("composer marker");
        assert!(
            bindings[0] < marker,
            "{surface} autocomplete binding must wrap its textarea"
        );
    }
}
