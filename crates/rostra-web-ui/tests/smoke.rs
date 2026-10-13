mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::TestServer;
use reqwest::header;
use rostra_core::event::{
    Event, EventKind, PersonaTag, PersonasTagsSelector, VerifiedEvent, VerifiedEventContent,
    content_kind,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, ExternalEventId, ShortEventId};
use scraper::{Html, Selector};
use serde_json::json;

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

#[tokio::test(flavor = "multi_thread")]
async fn profile_tab_uses_viewed_profile_summary_name_and_fallback() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (viewer, secret) = driver.login_new_identity().await;
    let target = retain_test_identity(&server, viewer).await;
    let hostile_name = "<img src=x onerror=alert(1)> & 世界";
    let response = driver
        .api_post_json(
            &format!("/api/{viewer}/update-social-profile-managed"),
            Some(&secret.to_string()),
            &json!({ "display_name": hostile_name, "bio": "" }),
        )
        .await;
    assert_eq!(response.status(), 200);

    for (id, expected) in [
        (viewer, hostile_name.to_owned()),
        (target, target.to_short().to_string()),
    ] {
        let response = driver.get(&format!("/profile/{}", id.to_short())).await;
        assert_eq!(response.status(), 200);
        let document = Html::parse_document(&response.text().await.unwrap());
        let tab = document
            .select(
                &Selector::parse(".o-mainBarTimeline__profile .o-mainBarTimeline__tabLabel")
                    .unwrap(),
            )
            .next()
            .unwrap();
        let summary = document
            .select(&Selector::parse("#profile-summary .m-profileSummary__displayName").unwrap())
            .next()
            .unwrap();
        assert_eq!(tab.text().collect::<String>(), expected);
        assert_eq!(
            tab.text().collect::<String>(),
            summary.text().collect::<String>()
        );
        assert_eq!(tab.child_elements().count(), 0);
        assert_eq!(
            document
                .select(&Selector::parse(".o-mainBarTimeline__feedLink").unwrap())
                .count(),
            1
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_summary_actions_are_only_rendered_on_profile_pages() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (viewer, _) = driver.login_new_identity().await;
    let target = retain_test_identity(&server, viewer).await;

    for path in ["/following", "/network", "/news", "/shoutbox"] {
        let response = driver.get(path).await;
        assert_eq!(response.status(), 200, "{path}");
        let document = Html::parse_document(&response.text().await.unwrap());
        assert!(
            document
                .select(&Selector::parse("#self-profile-summary a[href]").unwrap())
                .next()
                .is_some(),
            "{path} must preserve the self-profile link"
        );
        assert!(
            document
                .select(&Selector::parse(".m-profileSummary__buttons").unwrap())
                .next()
                .is_none(),
            "{path} must omit the action container"
        );
    }

    for id in [viewer, target] {
        let response = driver.get(&format!("/profile/{}", id.to_short())).await;
        assert_eq!(response.status(), 200);
        let document = Html::parse_document(&response.text().await.unwrap());
        let copy = document
            .select(
                &Selector::parse(
                    "#profile-summary .m-profileSummary__identity button[aria-label='Copy RostraId']",
                )
                .unwrap(),
            )
            .next()
            .unwrap();
        assert_eq!(copy.value().attr("type"), Some("button"));
        assert_eq!(
            copy.value().attr("data-value"),
            Some(id.to_string().as_str())
        );
        assert_eq!(
            copy.value().attr("onclick"),
            Some("copyIdToClipboard(event)")
        );
        assert!(copy.text().all(|text| text.trim().is_empty()));
        let message =
            Selector::parse("#profile-summary a.m-profileSummary__messageButton").unwrap();
        let follow = Selector::parse("#profile-summary form[method='get']").unwrap();
        assert_eq!(document.select(&message).next().is_some(), id != viewer);
        assert_eq!(document.select(&follow).next().is_some(), id != viewer);
        let actions = document
            .select(&Selector::parse("#profile-summary > .m-profileSummary__buttons").unwrap())
            .next();
        assert_eq!(actions.is_some(), id != viewer);
        if let Some(actions) = actions {
            let children: Vec<_> = actions.child_elements().collect();
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].value().name(), "form");
            assert_eq!(children[1].value().name(), "a");
            assert_eq!(
                children[0].value().attr("action"),
                Some(format!("/profile/{}/follow", id.to_short()).as_str())
            );
            assert_eq!(
                children[0].value().attr("x-target"),
                Some("follow-dialog-content")
            );
            let button = children[0]
                .select(&Selector::parse("button").unwrap())
                .next()
                .unwrap();
            assert!(button.text().any(|text| text == "Follow..."));
            assert!(button.value().attr("disabled").is_none());
        }
    }

    let readonly = server.driver();
    readonly.login_readonly(viewer).await;
    let response = readonly
        .get(&format!("/profile/{}", target.to_short()))
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    assert!(
        document
            .select(
                &Selector::parse("#profile-summary .m-profileSummary__followButton[disabled]")
                    .unwrap()
            )
            .next()
            .is_some()
    );
    assert!(
        document
            .select(&Selector::parse("#profile-summary .m-profileSummary__messageButton").unwrap())
            .next()
            .is_some(),
        "the message link retains its destination's server-side authorization"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn navigation_tabs_have_icons_and_accessible_labels_without_javascript() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, _) = driver.login_new_identity().await;

    let response = driver.get("/following").await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());

    for (selector, label) in [
        (".o-topNav__item[href='/']", "Home"),
        (
            ".o-topNav__item[href='https://github.com/dpc/rostra/discussions']",
            "Support",
        ),
        (".o-topNav__item[href='/settings/profile']", "Settings"),
        (".o-mainBarTimeline__followees", "Following"),
        (".o-mainBarTimeline__network", "Network"),
        (".o-mainBarTimeline__news", "News"),
        (".o-mainBarTimeline__notifications", "Notifications"),
        (".o-mainBarTimeline__shoutbox", "Shoutbox"),
        (".o-mainBarTimeline__messages", "Messages"),
    ] {
        let item = document
            .select(&Selector::parse(selector).unwrap())
            .next()
            .unwrap_or_else(|| panic!("missing navigation item {selector}"));
        assert_eq!(
            item.value().attr("aria-label"),
            None,
            "{label} should use its descendant text as its accessible name"
        );
        assert!(
            item.select(&Selector::parse("[aria-hidden='true']").unwrap())
                .next()
                .is_some(),
            "{label} should have a decorative icon"
        );
        assert!(
            item.text().any(|text| text.trim() == label),
            "{label} should retain its server-rendered visible label"
        );
    }

    let notifications = document
        .select(&Selector::parse(".o-mainBarTimeline__notifications").unwrap())
        .next()
        .unwrap();
    assert!(
        notifications
            .select(&Selector::parse(".o-mainBarTimeline__pendingNotifications[x-text]").unwrap())
            .next()
            .is_some(),
        "the dynamic notification count should remain inside the link's accessible name"
    );

    for path in [
        "/shoutbox".to_owned(),
        format!("/profile/{}", id.to_short()),
    ] {
        let response = driver.get(&path).await;
        assert_eq!(response.status(), 200, "{path}");
        let document = Html::parse_document(&response.text().await.unwrap());
        assert!(
            document
                .select(&Selector::parse(".o-mainBarTimeline__tabIcon").unwrap())
                .next()
                .is_some(),
            "{path} should render tab icons"
        );
        if path == "/shoutbox" {
            let messages = document
                .select(&Selector::parse(".o-mainBarTimeline__messages[href='/messages']").unwrap())
                .next()
                .expect("shoutbox should link to private messages");
            assert!(
                messages
                    .select(
                        &Selector::parse(
                            ".o-mainBarTimeline__tabIcon.-messages[aria-hidden='true']",
                        )
                        .unwrap(),
                    )
                    .next()
                    .is_some()
            );
            assert!(messages.text().any(|text| text.trim() == "Messages"));
            assert_eq!(messages.value().attr("aria-label"), None);
        }
    }

    let response = driver.get("/messages").await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    for (href, label) in [
        ("/", "Home"),
        ("https://github.com/dpc/rostra/discussions", "Support"),
        ("/settings/profile", "Settings"),
    ] {
        let selector = Selector::parse(&format!(".o-topNav a[href='{href}']")).unwrap();
        let item = document
            .select(&selector)
            .next()
            .unwrap_or_else(|| panic!("missing private-message navigation item {href}"));
        assert!(
            item.select(&Selector::parse("[aria-hidden='true']").unwrap())
                .next()
                .is_some(),
            "{label} should have a decorative icon"
        );
        assert!(item.text().any(|text| text.trim() == label));
    }
    for (href, label) in [
        ("/following", "Following"),
        ("/network", "Network"),
        ("/news", "News"),
        ("/notifications", "Notifications"),
        ("/shoutbox", "Shoutbox"),
        ("/messages", "Messages"),
    ] {
        let selector =
            Selector::parse(&format!(".o-mainBarTimeline__tabs a[href='{href}']")).unwrap();
        let item = document
            .select(&selector)
            .next()
            .unwrap_or_else(|| panic!("missing private-message top-level tab {href}"));
        assert!(item.text().any(|text| text.trim() == label));
    }
    assert!(
        document
            .select(&Selector::parse(".m-directMessages__conversationPanel").unwrap())
            .next()
            .is_some()
    );
    assert!(
        document
            .select(
                &Selector::parse(
                    ".o-mainBarTimeline__messages.-active[href='/messages'][aria-current='page']",
                )
                .unwrap(),
            )
            .next()
            .is_some()
    );
    let response = driver.get("/settings/messages").await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    assert!(
        document
            .select(
                &Selector::parse(".o-settingsNav__item.-active[href='/settings/messages']",)
                    .unwrap(),
            )
            .next()
            .is_some()
    );

    let generic_tab_bar =
        Html::parse_fragment(&rostra_web_ui::UiState::render_page_tab_bar("Post").into_string());
    let back = generic_tab_bar
        .select(&Selector::parse(".o-mainBarTimeline__back").unwrap())
        .next()
        .unwrap();
    assert_eq!(back.value().attr("aria-label"), Some("Back"));
    assert_eq!(back.value().attr("title"), Some("Back"));
    assert!(
        back.select(&Selector::parse(".o-mainBarTimeline__tabIcon.-back").unwrap())
            .next()
            .is_some()
    );

    let stylesheet = include_str!("../assets/style.css");
    for (tab, icon) in [
        ("followees", "star"),
        ("network", "users"),
        ("news", "newspaper"),
        ("shoutbox", "bullhorn"),
    ] {
        assert!(
            stylesheet.contains(&format!(
                ".o-mainBarTimeline__tabIcon.-{tab} {{\n  background: url('/assets/icons/{icon}.svg')"
            )),
            "{tab} should use the {icon} icon"
        );
        let response = driver.get(&format!("/assets/icons/{icon}.svg")).await;
        assert_eq!(response.status(), 200, "{icon} should be served");
        assert!(
            response.text().await.unwrap().contains("<svg"),
            "{icon} should contain SVG markup"
        );
    }
    assert_ne!(
        stylesheet
            .split_once(".o-mainBarTimeline__tabIcon.-shoutbox {")
            .unwrap()
            .1
            .split_once('}')
            .unwrap()
            .0,
        stylesheet
            .split_once(".o-mainBarTimeline__tabIcon.-messages {")
            .unwrap()
            .1
            .split_once('}')
            .unwrap()
            .0,
        "shoutbox and Messages should use distinct icons"
    );
    assert!(stylesheet.contains("@container (max-width: 15.625rem)"));
    assert!(stylesheet.contains("@media (max-width: 32rem)"));
    assert!(stylesheet.contains("@container (max-width: 48.75rem)"));
    assert!(stylesheet.contains("clip-path: inset(50%)"));
    assert!(stylesheet.contains(
        ".o-topNav__item {\n    min-height: 2.75rem;\n    justify-content: center;\n  }"
    ));
    assert!(stylesheet.contains(
        ".o-mainBarTimeline__tabs>* {\n  display: inline-flex;\n  flex: 1 1 auto;\n  align-items: center;\n  justify-content: center;\n  gap: 0.25rem;\n  font-weight: 600;\n\n  min-height: 2.5rem;"
    ));
    assert!(
        !stylesheet.contains(".m-directMessagesLayout > .m-directMessages__sidebar > .o-topNav"),
        "Messages must not carry separate top-navigation sizing"
    );
    assert!(
        !stylesheet.contains(".m-directMessagesLayout .o-mainBarTimeline__tabs"),
        "Messages must not carry separate timeline-navigation sizing"
    );
    assert!(stylesheet.contains(
        ".o-mainBarTimeline__tabs>.o-mainBarTimeline__back,\n.o-mainBarTimeline__tabs>.o-mainBarTimeline__feedLink {\n  flex: 0 0 auto;\n  padding-inline: 1rem;\n}"
    ));
    assert!(stylesheet.contains(
        ".o-mainBarTimeline__pendingNotifications,\n.o-mainBarTimeline__newCount {\n  padding: 0 .1rem;\n}"
    ));
    assert!(stylesheet.contains(
        ".o-shoutbox {\n  display: flex;\n  flex-direction: column;\n  container-type: inline-size;"
    ));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_diagnostics_are_authenticated_read_only_and_session_scoped() {
    let server = TestServer::start().await;
    let anonymous = server.driver();
    assert!(
        anonymous
            .get("/settings/retention")
            .await
            .status()
            .is_redirection()
    );
    let rw = server.driver();
    let (id, _) = rw.login_new_identity().await;
    let db = server.client(id).await;
    let before = db.db().get_payload_usage().await.unwrap();
    let response = rw.get("/settings/retention?id=untrusted").await;
    assert!(response.status().is_success());
    assert!(
        response.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let page = response.text().await.unwrap();
    assert!(page.contains("<html"));
    assert!(page.contains(&id.to_string()));
    assert!(page.contains("Startup mode: Disabled"));
    assert!(page.contains("No DryRun report available"));
    assert!(page.contains("Physical database size and reclamation: unknown"));
    assert_eq!(before, db.db().get_payload_usage().await.unwrap());
    let ro = server.driver();
    ro.login_readonly(id).await;
    assert!(!ro.get("/settings/retention").await.status().is_success());
    drop(db);
    server.shutdown().await;
}

fn assert_untrusted_media_headers(response: &reqwest::Response, content_type: &str) {
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        content_type
    );
    assert_eq!(
        response
            .headers()
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .unwrap(),
        "nosniff"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap(),
        "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'"
    );
}

fn assert_attachment_headers(response: &reqwest::Response) {
    assert_untrusted_media_headers(response, "application/octet-stream");
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
        r#"attachment; filename="rostra-media.bin""#
    );
}

async fn publish_social_post(
    driver: &common::UiDriver,
    author: RostraId,
    secret: &RostraIdSecretKey,
    parent_head_id: &str,
    content: &str,
    reply_to: Option<String>,
) -> String {
    let mut body = json!({
        "parent_head_id": parent_head_id,
        "content": content,
    });
    if let Some(reply_to) = reply_to {
        body["reply_to"] = reply_to.into();
    }
    let response = driver
        .api_post_json(
            &format!("/api/{author}/publish-social-post-managed"),
            Some(&secret.to_string()),
            &body,
        )
        .await;
    assert_eq!(response.status(), 200);
    response.json::<serde_json::Value>().await.unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn json_ld(document: &Html) -> serde_json::Value {
    let selector = Selector::parse("script[type='application/ld+json']").unwrap();
    let mut scripts = document.select(&selector);
    let payload = scripts
        .next()
        .expect("page should contain JSON-LD")
        .text()
        .collect::<String>();

    assert!(
        scripts.next().is_none(),
        "page should contain one JSON-LD script"
    );
    assert!(
        !payload.contains('<'),
        "serialized JSON-LD must not contain HTML script-data delimiters"
    );
    serde_json::from_str(&payload).expect("JSON-LD should remain valid JSON")
}

fn assert_json_ld_did_not_create_markup(document: &Html) {
    assert_eq!(
        document
            .select(
                &Selector::parse(
                    "script:not([src]):not(.mathjax):not([type='application/ld+json'])",
                )
                .unwrap(),
            )
            .count(),
        0,
        "JSON-LD content must not create another script element"
    );
    assert_eq!(
        document
            .select(&Selector::parse("script[src='/assets/app.js']").unwrap())
            .count(),
        1,
        "resources after JSON-LD should remain in the parsed head"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn profile_and_post_json_ld_keep_hostile_profile_text_as_data() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (author, secret) = driver.login_new_identity().await;
    let display_name = "</script><ScRiPt>alert(1)</sCrIpT><!-- <script";
    let bio = "quotes: \" backslash: \\ ampersand: & unicode: 世界 literal: \\u003c";

    let response = driver
        .api_post_json(
            &format!("/api/{author}/update-social-profile-managed"),
            Some(&secret.to_string()),
            &json!({
                "display_name": display_name,
                "bio": bio,
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let profile_update: serde_json::Value = response.json().await.unwrap();
    let profile_head = profile_update["event_id"].as_str().unwrap();

    let profile_path = format!("/profile/{}", author.to_short());
    let response = driver.get(&profile_path).await;
    assert_eq!(response.status(), 200);
    let profile_page = Html::parse_document(&response.text().await.unwrap());
    let profile_json_ld = json_ld(&profile_page);
    assert_eq!(profile_json_ld["name"], display_name);
    assert_eq!(profile_json_ld["description"], bio);
    assert_eq!(profile_json_ld["mainEntity"]["name"], display_name);
    assert_eq!(profile_json_ld["mainEntity"]["description"], bio);
    assert_json_ld_did_not_create_markup(&profile_page);

    let response = driver.ajax_get(&profile_path).await;
    assert_eq!(response.status(), 200);
    assert!(
        !response
            .text()
            .await
            .unwrap()
            .contains("application/ld+json"),
        "AJAX profile fragments should not include document metadata"
    );

    let post_id = publish_social_post(
        &driver,
        author,
        &secret,
        profile_head,
        "# Structured heading\n\nPost body",
        None,
    )
    .await;
    let post_path = format!("/post/{}/{post_id}", author.to_short());
    let response = driver.get(&post_path).await;
    assert_eq!(response.status(), 200);
    let post_page = Html::parse_document(&response.text().await.unwrap());
    assert!(
        post_page
            .select(&Selector::parse("#profile-summary a[href]").unwrap())
            .next()
            .is_some(),
        "post pages must preserve the author profile link"
    );
    assert!(
        post_page
            .select(&Selector::parse(".m-profileSummary__buttons").unwrap())
            .next()
            .is_none(),
        "post pages must omit profile actions"
    );
    let post_json_ld = json_ld(&post_page);
    assert_eq!(post_json_ld["headline"], "Structured heading");
    assert_eq!(post_json_ld["author"]["name"], display_name);
    assert_json_ld_did_not_create_markup(&post_page);

    let raw_post_path = format!("{post_path}?raw=true");
    for path in [&post_path, &raw_post_path] {
        let response = if path.ends_with("raw=true") {
            driver.get(path).await
        } else {
            driver.ajax_get(path).await
        };
        assert_eq!(response.status(), 200);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("application/ld+json"),
            "post fragments should not include document metadata"
        );
    }
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
    let document = Html::parse_document(&body);
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/following"]"#).unwrap())
            .next()
            .is_some(),
        "landing actions should link directly to the canonical timeline"
    );
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/home"]"#).unwrap())
            .next()
            .is_none(),
        "landing actions should not link to the legacy home redirect"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn landing_with_default_profile_links_to_following() {
    let server = TestServer::start_with_default_profile(RostraIdSecretKey::generate().id()).await;
    let driver = server.driver();

    let response = driver.get("/").await;
    assert_eq!(response.status(), 200);

    let document = Html::parse_document(&response.text().await.unwrap());
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/following"]"#).unwrap())
            .next()
            .is_some(),
        "Explore should link directly to the canonical timeline"
    );
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/home"]"#).unwrap())
            .next()
            .is_none(),
        "Explore should not link to the legacy home redirect"
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
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/following");

    let resp = driver.get("/home").await;
    assert_eq!(resp.status(), 308);
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/following");

    let resp = driver.get("/following").await;
    assert_eq!(resp.status(), 200);

    let document = Html::parse_document(&resp.text().await.unwrap());
    assert_link_precedes(&document, "/following", "/news");
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/settings/profile"]"#).unwrap())
            .next()
            .is_some(),
        "timeline navigation should link directly to the canonical settings page"
    );
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/settings"]"#).unwrap())
            .next()
            .is_none(),
        "timeline navigation should not link to the settings redirect"
    );

    let response = driver.get("/settings/profile").await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/following"]"#).unwrap())
            .next()
            .is_some(),
        "Settings Back should link directly to the canonical timeline"
    );
    assert!(
        document
            .select(&Selector::parse(r#"a[href="/home"]"#).unwrap())
            .next()
            .is_none(),
        "Settings Back should not link to the legacy home redirect"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn ordinary_add_followee_redirects_to_following_settings() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, _) = driver.login_new_identity().await;
    let followee = RostraIdSecretKey::generate().id();
    let followee_id = followee.to_string();

    let response = driver
        .post_form("/followee", &[("rostra_id", &followee_id)])
        .await;
    assert_eq!(response.status(), 303);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/settings/following"
    );

    let response = driver.get("/settings/following").await;
    assert_eq!(response.status(), 200);
    let page = response.text().await.unwrap();
    assert!(page.contains("<html"));
    assert!(page.contains(&followee_id));

    let followees = server.client(id).await.db().get_followees(id).await;
    assert!(
        followees
            .iter()
            .any(|(followee_id, _)| *followee_id == followee)
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn ajax_add_followee_returns_form_fragment_after_following() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, _) = driver.login_new_identity().await;
    let followee = RostraIdSecretKey::generate().id();
    let followee_id = followee.to_string();

    let response = driver
        .ajax_post_form("/followee", &[("rostra_id", &followee_id)])
        .await;
    assert_eq!(response.status(), 200);
    let fragment = response.text().await.unwrap();
    assert!(fragment.contains("id=\"add-followee-form\""));
    assert!(fragment.contains("Followed!"));
    assert!(!fragment.contains("<html"));

    let followees = server.client(id).await.db().get_followees(id).await;
    assert!(
        followees
            .iter()
            .any(|(followee_id, _)| *followee_id == followee)
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn ordinary_follow_editor_supports_native_navigation_and_custom_tags() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (viewer, secret) = driver.login_new_identity().await;
    let target = retain_test_identity(&server, viewer).await;
    let client = server.client(viewer).await;
    client
        .follow(
            secret,
            target,
            PersonasTagsSelector::Except {
                ids: BTreeSet::new(),
            },
        )
        .await
        .unwrap();

    let follow_action = format!("/profile/{}/follow", target.to_short());
    for path in [
        format!("/profile/{}", target.to_short()),
        "/settings/following".to_owned(),
    ] {
        let response = driver.get(&path).await;
        assert_eq!(response.status(), 200);
        let document = Html::parse_document(&response.text().await.unwrap());
        let form = document
            .select(
                &Selector::parse(&format!(r#"form[action="{follow_action}"][method="get"]"#))
                    .unwrap(),
            )
            .next()
            .unwrap_or_else(|| panic!("{path} lacks the ordinary follow editor entry"));
        assert!(
            form.select(
                &Selector::parse(r#"input[type="hidden"][name="following"][value="true"]"#)
                    .unwrap()
            )
            .next()
            .is_some()
        );
    }

    let legacy_path = format!("/profile/{target}/follow?following=false");
    let response = driver.get(&legacy_path).await;
    assert_eq!(response.status(), 308);
    let canonical_path = format!("/profile/{}/follow?following=false", target.to_short());
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap(),
        canonical_path
    );

    let response = driver.get(&canonical_path).await;
    assert_eq!(response.status(), 200);
    let page = response.text().await.unwrap();
    assert!(page.contains("<html"));
    let document = Html::parse_document(&page);
    let profile_path = format!("/profile/{}", target.to_short());
    let form_selector =
        Selector::parse(&format!(r#"form[action="{follow_action}"][method="post"]"#)).unwrap();
    let form = document.select(&form_selector).next().unwrap();
    assert_eq!(form.value().attr("x-target"), None);
    assert_eq!(form.value().attr("@ajax:before"), None);
    assert_eq!(form.value().attr("@ajax:after"), None);
    assert!(
        form.select(&Selector::parse(r#"input[type="text"][name="personas"]"#).unwrap())
            .next()
            .is_some()
    );
    assert!(
        document
            .select(&Selector::parse(&format!(r#"a[href="{profile_path}"]"#)).unwrap())
            .any(|link| link.text().any(|text| text.trim() == "Back"))
    );
    assert!(
        document
            .select(&Selector::parse(r#"script[src="/assets/app.js"]"#).unwrap())
            .next()
            .is_some()
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    let response = driver
        .post_form(
            &format!("/profile/{target}/follow"),
            &[
                ("follow_type", "follow_only"),
                ("personas", "personal"),
                ("personas", "futuretag"),
                ("personas", ""),
            ],
        )
        .await;
    assert_eq!(response.status(), 303);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap(),
        profile_path
    );
    let followees = client.db().get_followees(viewer).await;
    assert_eq!(
        followees
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, selector)| selector),
        Some(&PersonasTagsSelector::Only {
            ids: BTreeSet::from([
                PersonaTag::new("futuretag").unwrap(),
                PersonaTag::personal(),
            ]),
        })
    );

    let response = driver
        .get(&format!(
            "/profile/{}/follow?following=true",
            target.to_short()
        ))
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let selected = document
        .select(
            &Selector::parse(
                r#"form input[type="checkbox"][name="personas"][value="futuretag"][checked]"#,
            )
            .unwrap(),
        )
        .count();
    assert_eq!(selected, 1);
    assert_eq!(
        document
            .select(&Selector::parse(r#"form input[type="text"][name="personas"]"#).unwrap())
            .count(),
        1
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    let response = driver
        .post_form(
            &format!("/profile/{}/follow", target.to_short()),
            &[("follow_type", "follow_only"), ("personas", "")],
        )
        .await;
    assert_eq!(response.status(), 303);
    let followees = client.db().get_followees(viewer).await;
    assert_eq!(
        followees
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, selector)| selector),
        Some(&PersonasTagsSelector::Only {
            ids: BTreeSet::new(),
        })
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    let response = driver
        .post_form(
            &format!("/profile/{}/follow", target.to_short()),
            &[
                ("follow_type", "follow_all"),
                ("personas", "professional"),
                ("personas", ""),
            ],
        )
        .await;
    assert_eq!(response.status(), 303);
    let followees = client.db().get_followees(viewer).await;
    assert_eq!(
        followees
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, selector)| selector),
        Some(&PersonasTagsSelector::Except {
            ids: BTreeSet::from([PersonaTag::professional()]),
        })
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    let response = driver
        .post_form(
            &format!("/profile/{}/follow", target.to_short()),
            &[("follow_type", "unfollow")],
        )
        .await;
    assert_eq!(response.status(), 303);
    assert!(
        !client
            .db()
            .get_followees(viewer)
            .await
            .iter()
            .any(|(id, _)| *id == target)
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn ajax_follow_editor_keeps_dialog_and_fragment_replacements() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (viewer, secret) = driver.login_new_identity().await;
    let target = retain_test_identity(&server, viewer).await;
    let canonical_path = format!("/profile/{}/follow", target.to_short());
    let client = server.client(viewer).await;
    client
        .follow(secret, target, PersonasTagsSelector::default())
        .await
        .unwrap();

    let response = driver
        .ajax_get(&format!("{canonical_path}?following=true"))
        .await;
    assert_eq!(response.status(), 200);
    let fragment = response.text().await.unwrap();
    assert!(!fragment.contains("<html"));
    let document = Html::parse_fragment(&fragment);
    let dialog = document
        .select(&Selector::parse("#follow-dialog-content.-active").unwrap())
        .next()
        .unwrap();
    let form = dialog
        .select(&Selector::parse("form[x-target]").unwrap())
        .next()
        .unwrap();
    assert_eq!(
        form.value().attr("x-target"),
        Some("profile-summary followee-list follower-list follow-dialog-content")
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    let response = driver
        .ajax_post_form(
            &canonical_path,
            &[("follow_type", "follow_all"), ("personas", "civic")],
        )
        .await;
    assert_eq!(response.status(), 200);
    let fragment = response.text().await.unwrap();
    for id in [
        "profile-summary",
        "followee-list",
        "follower-list",
        "follow-dialog-content",
    ] {
        assert!(
            fragment.contains(&format!("id=\"{id}\"")),
            "missing AJAX replacement {id}"
        );
    }
    let followees = client.db().get_followees(viewer).await;
    assert_eq!(
        followees
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, selector)| selector),
        Some(&PersonasTagsSelector::Except {
            ids: BTreeSet::from([PersonaTag::civic()]),
        })
    );

    let readonly = server.driver();
    readonly.login_readonly(viewer).await;
    let response = readonly
        .post_form(
            &canonical_path,
            &[("follow_type", "follow_only"), ("personas", "personal")],
        )
        .await;
    assert!(!response.status().is_success());
    let followees = client.db().get_followees(viewer).await;
    assert_eq!(
        followees
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, selector)| selector),
        Some(&PersonasTagsSelector::Except {
            ids: BTreeSet::from([PersonaTag::civic()]),
        })
    );
}

async fn retain_test_identity(server: &TestServer, viewer: RostraId) -> RostraId {
    let target_secret = RostraIdSecretKey::generate();
    let target = target_secret.id();
    let author = server.client(target).await;
    let post = author
        .social_post(
            target_secret,
            "Retained identity fixture".to_owned(),
            None,
            BTreeSet::new(),
        )
        .await
        .unwrap();
    let content = author.db().get_event_content(post.event_id).await.unwrap();
    let content = VerifiedEventContent::verify(post, content).unwrap();
    server
        .client(viewer)
        .await
        .store_event_with_content(content.event_id(), &content)
        .await
        .unwrap();
    target
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn timelines_support_head_and_followees_query_pagination() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let (viewer_id, viewer_secret) = driver.login_new_identity().await;
    let author_secret = RostraIdSecretKey::generate();
    let author_id = author_secret.id();
    let author = server.client(author_id).await;
    let older = author
        .social_post(
            author_secret,
            "older timeline post".to_string(),
            None,
            BTreeSet::new(),
        )
        .await
        .expect("publish older post");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let newer = author
        .social_post(
            author_secret,
            "newer timeline post".to_string(),
            None,
            BTreeSet::new(),
        )
        .await
        .expect("publish newer post");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let latest = author
        .social_post(
            author_secret,
            "latest timeline post".to_string(),
            None,
            BTreeSet::new(),
        )
        .await
        .expect("publish latest post");
    let viewer = server.client(viewer_id).await;
    viewer
        .follow(
            viewer_secret,
            author_id,
            PersonasTagsSelector::Except {
                ids: BTreeSet::new(),
            },
        )
        .await
        .expect("follow post author");
    for post in [older, newer, latest] {
        let content = author
            .db()
            .get_event_content(post.event_id)
            .await
            .expect("get post content");
        let content = VerifiedEventContent::verify(post, content).expect("verify post content");
        viewer
            .store_event_with_content(content.event_id(), &content)
            .await
            .expect("store followed post");
    }

    for path in ["/following", "/network", "/news", "/notifications"] {
        let response = driver.head(path).await;
        assert_eq!(response.status(), 200, "HEAD {path} should be successful");
    }

    let newer = viewer
        .db()
        .get_social_post(newer.event_id.into())
        .await
        .expect("newer post exists");
    let response = driver
        .get(&format!(
            "/following?ts={}&event_id={}",
            newer.ts,
            newer.event_id.to_short()
        ))
        .await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("older timeline post"),
        "a complete query cursor should retain posts before the cursor"
    );
    assert!(
        !body.contains("latest timeline post"),
        "a complete query cursor should exclude posts after the cursor"
    );
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
    assert!(
        !body.contains("/home</loc>"),
        "sitemap should omit legacy /home"
    );
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
async fn reaction_picker_updates_the_current_post_through_hypermedia() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (author, secret) = driver.login_new_identity().await;
    let client = server.client(author).await;
    let original = client
        .social_post(secret, "original post".to_owned(), None, BTreeSet::new())
        .await
        .expect("publish original post");
    let original_target = ExternalEventId::new(author, original.event_id.to_short());
    let old_reaction = client
        .social_post(
            secret,
            "👍".to_owned(),
            Some(original_target),
            BTreeSet::new(),
        )
        .await
        .expect("publish reaction to original post");
    let edited = client
        .publish_event(
            secret,
            content_kind::SocialPost::new_text("edited post".to_owned(), None, BTreeSet::new()),
        )
        .replace(original.event_id.to_short())
        .call()
        .await
        .expect("replace original post");
    let edited_id = edited.event_id.to_short();
    let post_path = format!("/post/{}/{edited_id}", author.to_short());

    let response = driver.get(&post_path).await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let like_button = Selector::parse(r#"button[aria-label="React to this post"]"#).unwrap();
    assert!(
        document.select(&like_button).next().is_some(),
        "rendered post should expose the labeled heart action"
    );
    let remove_reaction_button =
        Selector::parse(r#"button[aria-label="Remove your 👍 reaction"]"#).unwrap();
    assert!(
        document.select(&remove_reaction_button).next().is_some(),
        "a reaction to an older post version should remain removable"
    );

    let thread_query = format!("post_thread_id={edited_id}");
    let heart_path = format!("{post_path}/react/heart?{thread_query}");
    let response = driver.ajax_get(&heart_path).await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_fragment(&response.text().await.unwrap());
    assert!(
        document
            .select(&Selector::parse("dialog#post-preview-dialog.-active").unwrap())
            .next()
            .is_some(),
        "enhanced reaction action should return an in-page picker"
    );
    let publish_form = Selector::parse(&format!(
        r#"form[action="{post_path}/react/heart"][method="post"][x-target~="post-preview-dialog"]"#
    ))
    .unwrap();
    assert!(
        document.select(&publish_form).next().is_some(),
        "picker should submit through hypermedia"
    );
    let choices = Selector::parse(r#"button[type="submit"][name="reaction"]"#).unwrap();
    let expected = ["❤️", "🚀", "🤣", "😢", "👀", "👍", "🎉", "🤔"];
    assert_eq!(
        document
            .select(&choices)
            .map(|button| {
                assert!(button.value().attr("aria-label").is_some());
                assert_eq!(
                    button.text().collect::<String>(),
                    button.value().attr("value").unwrap()
                );
                button.value().attr("value").unwrap()
            })
            .collect::<Vec<_>>(),
        expected
    );

    let response = driver.get(&heart_path).await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let ordinary_form = Selector::parse(&format!(
        r#"form[action="{post_path}/react/heart"][method="post"]"#
    ))
    .unwrap();
    let ordinary_form = document
        .select(&ordinary_form)
        .next()
        .expect("ordinary picker form");
    assert!(
        ordinary_form.value().attr("x-target").is_none(),
        "ordinary picker must not require JavaScript"
    );
    assert_eq!(ordinary_form.select(&choices).count(), 8);

    let response = driver
        .ajax_post_form(
            &format!("{post_path}/react/heart"),
            &[
                ("post_thread_id", &edited_id.to_string()),
                ("reaction", "not an emoji"),
            ],
        )
        .await;
    assert_eq!(
        response.status(),
        400,
        "only offered reactions may be posted"
    );

    let response = driver
        .ajax_post_form(
            &format!("{post_path}/react/heart"),
            &[
                ("post_thread_id", &edited_id.to_string()),
                ("reaction", "🚀"),
            ],
        )
        .await;
    assert_eq!(response.status(), 200);
    let response_body = response.text().await.unwrap();
    assert!(response_body.contains(&format!(r#"id="post-reactions-{edited_id}-{edited_id}""#)));
    let document = Html::parse_fragment(&response_body);
    assert!(
        document
            .select(&Selector::parse("div#post-preview-dialog:not(.-active)").unwrap())
            .next()
            .is_some(),
        "successful hypermedia action should close the picker"
    );
    let (reactions, _) = client
        .db()
        .paginate_social_post_reactions_rev(edited_id, None, 1000)
        .await;
    assert_eq!(reactions.len(), 2, "one click should publish one reaction");
    assert!(
        reactions
            .iter()
            .any(|reaction| reaction.content.get_reaction() == Some("🚀"))
    );

    for emoji in expected {
        let response = driver
            .post_form(
                &format!("{post_path}/react/heart"),
                &[
                    ("post_thread_id", &edited_id.to_string()),
                    ("reaction", emoji),
                ],
            )
            .await;
        assert_eq!(response.status(), 303, "ordinary emoji action redirects");
    }
    let (reactions, _) = client
        .db()
        .paginate_social_post_reactions_rev(edited_id, None, 1000)
        .await;
    for emoji in expected {
        assert!(
            reactions
                .iter()
                .any(|reaction| reaction.content.get_reaction() == Some(emoji))
        );
    }
    let readonly = server.driver();
    readonly.login_readonly(author).await;
    for ajax in [false, true] {
        let response = if ajax {
            readonly.ajax_get(&heart_path).await
        } else {
            readonly.get(&heart_path).await
        };
        assert_eq!(response.status(), 200);
        let document = Html::parse_document(&response.text().await.unwrap());
        assert_eq!(
            document
                .select(&Selector::parse(r#"button[name="reaction"][disabled]"#).unwrap())
                .count(),
            8,
            "a read-only session must not offer enabled reaction actions"
        );
    }
    let before_readonly_post = reactions.len();
    let response = readonly
        .ajax_post_form(&format!("{post_path}/react/heart"), &[("reaction", "🚀")])
        .await;
    assert!(!response.status().is_success());
    let (reactions, _) = client
        .db()
        .paginate_social_post_reactions_rev(edited_id, None, 1000)
        .await;
    assert_eq!(reactions.len(), before_readonly_post);

    let delete_path = format!(
        "{post_path}/reaction/{}/delete?{thread_query}",
        old_reaction.event_id.to_short()
    );
    let response = driver.ajax_get(&delete_path).await;
    assert_eq!(
        response.status(),
        200,
        "reaction to the replaced post version should have a working confirmation"
    );
    let document = Html::parse_fragment(&response.text().await.unwrap());
    let remove_form = Selector::parse(&format!(
        r#"form[action="{post_path}/reaction/{}/delete"][method="post"][x-target~="post-preview-dialog"]"#,
        old_reaction.event_id.to_short()
    ))
    .unwrap();
    assert!(document.select(&remove_form).next().is_some());

    let response = driver
        .ajax_post_form(
            &format!(
                "{post_path}/reaction/{}/delete",
                old_reaction.event_id.to_short()
            ),
            &[("post_thread_id", &edited_id.to_string())],
        )
        .await;
    assert_eq!(response.status(), 200);
    let (reactions, _) = client
        .db()
        .paginate_social_post_reactions_rev(edited_id, None, 1000)
        .await;
    assert!(
        reactions
            .iter()
            .all(|reaction| reaction.event_id != old_reaction.event_id.to_short()),
        "confirmed deletion should remove the precise selected reaction"
    );

    server.shutdown().await;
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn focused_post_page_does_not_link_the_focused_post() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (author, secret) = driver.login_new_identity().await;
    let author_short = author.to_short();

    let response = driver.api_get(&format!("/api/{author}/heads")).await;
    let parent_head = response.json::<serde_json::Value>().await.unwrap()["heads"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let parent_id =
        publish_social_post(&driver, author, &secret, &parent_head, "parent post", None).await;
    let focused_id = publish_social_post(
        &driver,
        author,
        &secret,
        &parent_id,
        "focused post",
        Some(format!("{author}-{parent_id}")),
    )
    .await;
    let reply_id = publish_social_post(
        &driver,
        author,
        &secret,
        &focused_id,
        "reply post",
        Some(format!("{author}-{focused_id}")),
    )
    .await;

    let response = driver
        .get(&format!("/post/{author_short}/{focused_id}"))
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let post_main = |event_id: &str| {
        let selector =
            Selector::parse(&format!("#post-{focused_id}-{event_id} .m-postView__main")).unwrap();
        document
            .select(&selector)
            .next()
            .unwrap_or_else(|| panic!("missing post {event_id}"))
    };

    let focused_post = post_main(&focused_id);
    assert_eq!(
        focused_post.value().attr("data-href"),
        None,
        "the focused post must not link to itself"
    );
    assert_eq!(focused_post.value().attr("@click"), None);
    assert_eq!(focused_post.value().attr("@auxclick"), None);
    for event_id in [parent_id, reply_id] {
        let post = post_main(&event_id);
        assert!(
            post.value().attr("data-href")
                == Some(format!("/post/{author_short}/{event_id}").as_str()),
            "contextual post {event_id} must retain its link"
        );
        for event in ["@click", "@auxclick"] {
            assert_eq!(post.value().attr(event), Some("navigatePost($event)"));
        }
    }
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
        if legacy_url.contains("/avatar") {
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=86400"
            );
        }
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
    assert_eq!(response.status(), 303);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!("/post/{}/{event_id}", author.to_short())
    );

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
    assert_eq!(response.status(), 303);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        &format!("/post/{}/{missing_event_id}", author.to_short())
    );

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
async fn secure_unlock_page_offers_in_place_account_creation() {
    let server = TestServer::start().await;
    let driver = server.driver();

    let resp = driver.get("/unlock?redirect=%2Ffollowing").await;
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
    assert!(!body.contains("Save recovery phrase"));
    assert!(!body.contains("account-recovery-target"));
    assert!(!body.contains("/unlock/generate"));
    assert!(!body.contains("recovery-phrase"));
    assert!(body.contains("name=\"redirect\" value=\"/following\""));

    let document = Html::parse_document(&body);
    let create_account_selector = Selector::parse("button[type='button']").unwrap();
    let create_account = document
        .select(&create_account_selector)
        .find(|button| button.text().collect::<String>().contains("Create Account"))
        .expect("Create Account button");
    assert_eq!(create_account.value().attr("type"), Some("button"));
    assert_eq!(create_account.value().attr("form"), None);
    assert!(create_account.value().attr("onclick").is_some());

    let login_form_selector = Selector::parse("form[action='/unlock'][method='post']").unwrap();
    let login_form = document
        .select(&login_form_selector)
        .next()
        .expect("ordinary login form");
    let credential_selector =
        Selector::parse("input[name='username'], input[name='password']").unwrap();
    assert_eq!(login_form.select(&credential_selector).count(), 2);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn normal_login_validates_local_redirects() {
    let server = TestServer::start().await;
    let driver = server.driver();
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
async fn account_generation_route_and_recovery_page_are_absent() {
    let server = TestServer::start().await;
    let driver = server.driver();

    assert_eq!(driver.get("/unlock/generate").await.status(), 404);
    assert_eq!(
        driver.post_form("/unlock/generate", &[]).await.status(),
        404
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn credential_export_requires_https_off_loopback() {
    let server = TestServer::start_non_loopback_http().await;
    let driver = server.driver();

    let page = driver.get("/unlock").await.text().await.unwrap();
    assert!(page.contains("Account creation is disabled"));
    let document = Html::parse_document(&page);
    let create_account_selector = Selector::parse("button[type='button']").unwrap();
    let create_account = document
        .select(&create_account_selector)
        .find(|button| button.text().collect::<String>().contains("Create Account"))
        .expect("Create Account button");
    assert!(create_account.value().attr("disabled").is_some());
    assert_eq!(create_account.value().attr("onclick"), None);

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

    let resp = driver.get("/unlock").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store, private"
    );
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL],
        "public, max-age=86400"
    );

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("Missing Content-Type")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "image/svg+xml");
    assert_untrusted_media_headers(&resp, "image/svg+xml");

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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL],
        "public, max-age=86400"
    );
    assert_untrusted_media_headers(&resp, "image/svg+xml");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn user_avatar_isolated_on_success_and_not_modified() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, secret) = driver.login_new_identity().await;
    let avatar = b"<svg><script>alert('xss')</script></svg>".to_vec();

    server
        .client(id)
        .await
        .post_social_profile_update(
            secret,
            "Test avatar".to_owned(),
            String::new(),
            Some(("image/svg+xml".to_owned(), avatar)),
        )
        .await
        .expect("publish test avatar");

    let path = format!("/profile/{}/avatar", id.to_short());
    let response = driver.get(&path).await;
    assert_eq!(response.status(), 200);
    assert_untrusted_media_headers(&response, "image/svg+xml");
    let etag = response
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap();

    let response = driver.get_if_none_match(&path, etag).await;
    assert_eq!(response.status(), 304);
    assert_untrusted_media_headers(&response, "image/svg+xml");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn hostile_media_downloads_on_success_and_not_modified() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, secret) = driver.login_new_identity().await;
    let event = server
        .client(id)
        .await
        .publish_event(
            secret,
            content_kind::SocialMedia {
                mime: "text/html".to_owned(),
                data: b"<script>alert('xss')</script>".to_vec(),
            },
        )
        .call()
        .await
        .expect("publish test media");

    let path = format!("/media/{}/{}", id.to_short(), event.event_id.to_short());
    let response = driver.get(&path).await;
    assert_eq!(response.status(), 200);
    assert_attachment_headers(&response);
    let etag = response
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap();

    let response = driver.get_if_none_match(&path, etag).await;
    assert_eq!(response.status(), 304);
    assert_attachment_headers(&response);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn validated_media_stays_inline_on_success_and_not_modified() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, secret) = driver.login_new_identity().await;
    let event = server
        .client(id)
        .await
        .publish_event(
            secret,
            content_kind::SocialMedia {
                mime: "image/png".to_owned(),
                data: b"\x89PNG\r\n\x1a\n".to_vec(),
            },
        )
        .call()
        .await
        .expect("publish test media");

    let path = format!("/media/{}/{}", id.to_short(), event.event_id.to_short());
    let response = driver.get(&path).await;
    assert_eq!(response.status(), 200);
    assert_untrusted_media_headers(&response, "image/png");
    assert!(
        response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .is_none()
    );
    let etag = response
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap();

    let response = driver.get_if_none_match(&path, etag).await;
    assert_eq!(response.status(), 304);
    assert_untrusted_media_headers(&response, "image/png");
    assert!(
        response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .is_none()
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn mismatched_media_downloads_instead_of_rendering_inline() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let (id, secret) = driver.login_new_identity().await;
    let event = server
        .client(id)
        .await
        .publish_event(
            secret,
            content_kind::SocialMedia {
                mime: "image/png".to_owned(),
                data: b"<script>alert('xss')</script>".to_vec(),
            },
        )
        .call()
        .await
        .expect("publish test media");

    let response = driver
        .get(&format!(
            "/media/{}/{}",
            id.to_short(),
            event.event_id.to_short()
        ))
        .await;
    assert_eq!(response.status(), 200);
    assert_attachment_headers(&response);
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

    let resp = driver
        .head(&format!("/profile/{}/avatar", id.to_short()))
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL],
        "public, max-age=86400"
    );
    assert_untrusted_media_headers(&resp, "image/svg+xml");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn avatar_authentication_and_lookup_failures_are_not_cacheable() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let missing_id = RostraIdSecretKey::generate().id();
    let path = format!("/profile/{}/avatar", missing_id.to_short());

    let response = driver.get(&path).await;
    assert_eq!(response.status(), 303);
    assert_eq!(
        response.headers()[header::LOCATION],
        format!(
            "/unlock?redirect=%2Fprofile%2F{}%2Favatar",
            missing_id.to_short()
        )
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );

    driver.login_new_identity().await;
    let response = driver.get(&path).await;
    assert_eq!(response.status(), 404);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
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
    assert_eq!(resp.status(), 303);

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
