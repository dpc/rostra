//! Disposable ordinary-HTTP DM workflows. Run locally in a loopback-only
//! network namespace; SelfCI runs the suite in the Nix build sandbox.

mod common;

use std::time::Duration;

use common::{TestServer, UiDriver};
use futures_util::StreamExt as _;
use reqwest::{Response, StatusCode, header};
use rostra_core::event::{VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, ToShort as _};
use scraper::{Html, Selector};

fn private_headers(response: &Response) {
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store, private"
    );
    assert_eq!(response.headers()[header::PRAGMA], "no-cache");
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "identity");
    let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("script-src 'self' 'unsafe-eval'"));
    assert!(csp.contains("connect-src 'self'"));
    assert!(csp.contains("style-src 'self' 'unsafe-inline'"));
    assert!(csp.contains("font-src 'self'"));
}

fn token(page: &str) -> String {
    let document = Html::parse_document(page);
    document
        .select(&Selector::parse("input[name=csrf]").unwrap())
        .next()
        .unwrap()
        .value()
        .attr("value")
        .unwrap()
        .to_owned()
}

async fn settings_token(driver: &UiDriver) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = driver.get("/settings/messages").await;
            assert_eq!(response.status(), StatusCode::OK);
            private_headers(&response);
            let page = response.text().await.unwrap();
            let document = Html::parse_document(&page);
            if document
                .select(&Selector::parse("input[name=csrf]").unwrap())
                .next()
                .is_some()
            {
                return token(&page);
            }
            if let Some(confirmation) = document
                .select(&Selector::parse("a[href^='/settings/messages/retire/']").unwrap())
                .next()
            {
                let response = driver.get(confirmation.value().attr("href").unwrap()).await;
                assert_eq!(response.status(), StatusCode::OK);
                private_headers(&response);
                return token(&response.text().await.unwrap());
            }
            // Enrollment persists the installation before publishing its device
            // announcement. During that interval neither lifecycle form exists.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("message-device enrollment should expose a lifecycle form")
}

async fn replicate_device(server: &TestServer, source: RostraId, target: RostraId) {
    let source_client = server.client(source).await;
    let event_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let devices = source_client.db().dm_own_devices(None, 64).await.unwrap();
            if let Some((_, event, _)) = devices.iter().find_map(|(_, device)| device.latest()) {
                break event;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    replicate_event(server, source, target, event_id).await;
}

async fn replicate_event(
    server: &TestServer,
    source: RostraId,
    target: RostraId,
    event_id: rostra_core::ShortEventId,
) {
    let source_client = server.client(source).await;
    let target_client = server.client(target).await;
    let record = source_client.db().get_event(event_id).await.unwrap();
    let event = VerifiedEvent::verify_signed(source, record.signed).unwrap();
    let content = source_client
        .db()
        .get_event_content(event_id)
        .await
        .unwrap();
    target_client
        .db()
        .try_process_event_with_content(&VerifiedEventContent::verify(event, content).unwrap())
        .await
        .unwrap();
}

fn assert_conversation_panel_keeps_start_form(document: &Html) {
    let panel = document
        .select(&Selector::parse(".m-directMessages__conversationPanel").unwrap())
        .next()
        .expect("conversation panel");
    assert!(
        panel
            .select(&Selector::parse("form[action='/messages/open']").unwrap())
            .next()
            .is_some(),
        "conversation panel should retain the ordinary start-conversation form"
    );
    assert!(
        panel
            .select(&Selector::parse("a[href='/settings/messages']").unwrap())
            .next()
            .is_none(),
        "device administration belongs in Settings, not the conversation workspace"
    );
    assert!(
        panel
            .select(&Selector::parse("h1").unwrap())
            .next()
            .is_none(),
        "the top-level Messages tab already identifies the workspace"
    );
}

#[test]
fn private_message_autocomplete_keeps_queries_out_of_request_urls() {
    let app = include_str!("../assets/app.js");
    assert!(app.contains("method: \"POST\""));
    assert!(app.contains("body: new URLSearchParams({"));
    assert!(app.contains("q: this.query"));
    assert!(
        !app.contains("`${options.profileSearchUrl}?"),
        "the private profile search URL must not contain composer text"
    );

    let messages = include_str!("../src/routes/messages.rs");
    assert!(messages.contains("profileSearchUrl: '/messages/profile-search'"));
    assert!(messages.contains("profileSearchCsrf:"));
}

#[tokio::test(flavor = "multi_thread")]
async fn private_workspace_uses_the_shared_shell_with_rich_content_resources() {
    let server = TestServer::start().await;
    let writer = server.driver();
    writer.login_new_identity().await;
    let following = Html::parse_document(&writer.get("/following").await.text().await.unwrap());
    let settings =
        Html::parse_document(&writer.get("/settings/profile").await.text().await.unwrap());
    let favicon = Selector::parse("head link[rel=icon]").unwrap();
    let runtime = Selector::parse("head script[src]").unwrap();
    let top_nav_container = Selector::parse(".o-topNav").unwrap();
    let top_nav = Selector::parse(".o-topNav a").unwrap();
    let tabs = Selector::parse(".o-mainBarTimeline__tabs a").unwrap();
    let settings_nav = Selector::parse(".o-settingsNav a").unwrap();
    let links = |document: &Html, selector: &Selector| {
        document
            .select(selector)
            .map(|link| link.value().attr("href").unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let top_nav_signature = |document: &Html| {
        document
            .select(&top_nav_container)
            .next()
            .expect("top navigation")
            .select(&Selector::parse("*").unwrap())
            .map(|element| {
                (
                    element.value().name().to_owned(),
                    element.value().attr("class").map(str::to_owned),
                    element.value().attr("href").map(str::to_owned),
                    element.value().attr("aria-hidden").map(str::to_owned),
                    element.text().collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let expected_top_nav = top_nav_signature(&following);
    let expected_runtime = following
        .select(&runtime)
        .filter_map(|script| script.value().attr("src"))
        .collect::<Vec<_>>();
    let expected_private_runtime = expected_runtime
        .iter()
        .copied()
        .filter(|src| !src.contains("/prismjs/") && !src.contains("/mathjax-"))
        .collect::<Vec<_>>();
    let mut expected_private_rich_runtime = expected_runtime.clone();
    expected_private_rich_runtime.push("/assets/emoji-init.js");
    for path in ["/messages", "/settings/messages", "/messages/not-an-id"] {
        let response = writer.get(path).await;
        private_headers(&response);
        let document = Html::parse_document(&response.text().await.unwrap());
        assert_eq!(links(&document, &favicon), links(&following, &favicon));
        assert_eq!(
            document
                .select(&runtime)
                .filter_map(|script| script.value().attr("src"))
                .collect::<Vec<_>>(),
            if path == "/settings/messages" {
                expected_private_runtime.clone()
            } else {
                expected_private_rich_runtime.clone()
            },
        );
        assert!(
            document
                .select(&Selector::parse("script:not([src]), style, [style]").unwrap())
                .next()
                .is_none()
        );
        assert!(
            document
                .select(&Selector::parse("body[x-data=notifications] .o-notificationArea").unwrap())
                .next()
                .is_some()
        );
        if path == "/settings/messages" {
            assert_eq!(
                links(&document, &settings_nav),
                links(&settings, &settings_nav)
            );
        } else {
            let message_sidebar =
                Selector::parse(".m-directMessages__sidebar > nav.o-navBar > .o-topNav").unwrap();
            let conversation_panel = Selector::parse(
                ".m-directMessages__sidebar > nav.m-directMessages__conversationPanel",
            )
            .unwrap();
            assert!(
                document.select(&message_sidebar).next().is_some(),
                "Messages must preserve the shared navbar box inside its ordering wrapper"
            );
            assert!(
                document.select(&conversation_panel).next().is_some(),
                "the conversation panel must remain a sibling of the shared navbar"
            );
            let timeline_tabs = document
                .select(
                    &Selector::parse(
                        "#direct-message-tabs.o-mainBarTimeline__tabs > .o-mainBarTimeline__back",
                    )
                    .unwrap(),
                )
                .next()
                .expect("shared timeline tab bar");
            assert_eq!(timeline_tabs.value().attr("href"), Some("/"));
            assert_eq!(
                timeline_tabs.value().attr("onclick"),
                None,
                "private pages must not add the public history-navigation handler"
            );
            assert_eq!(
                top_nav_signature(&document),
                expected_top_nav,
                "Messages must use the same rendered top navigation as ordinary pages"
            );
            assert_eq!(links(&document, &top_nav), links(&following, &top_nav));
            assert_eq!(links(&document, &tabs), links(&following, &tabs));
        }
        if path == "/messages/not-an-id" {
            assert!(
                document
                    .select(&Selector::parse("#direct-message-thread.-open [role=alert]").unwrap())
                    .next()
                    .is_some(),
                "private errors must remain visible in the mobile thread layout"
            );
        }
    }
    let css = writer.get("/assets/nojs.css").await;
    assert_eq!(css.status(), StatusCode::OK);
    assert!(css.text().await.unwrap().contains(".u-requiresJs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn private_pages_require_this_sessions_secret_and_protect_all_responses() {
    let server = TestServer::start().await;
    let anonymous = server.driver();
    for (path, location) in [
        ("/messages", "/unlock?redirect=%2Fmessages"),
        (
            "/settings/messages",
            "/unlock?redirect=%2Fsettings%2Fmessages",
        ),
        (
            "/messages/not-an-id",
            "/unlock?redirect=%2Fmessages%2Fnot-an-id",
        ),
        (
            "/messages/open?peer=not-an-id",
            "/unlock?redirect=%2Fmessages%2Fopen%3Fpeer%3Dnot-an-id",
        ),
    ] {
        let response = anonymous.get(path).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], location);
        private_headers(&response);
    }
    let writer = server.driver();
    let (id, secret) = writer.login_new_identity().await;
    let observer = server.driver();
    observer.login_readonly(id).await;
    // The shared client is fully active because of the other session.
    for path in ["/messages", "/settings/messages"] {
        let response = observer.get(path).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        private_headers(&response);
    }
    let response = writer.get("/messages").await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let page = response.text().await.unwrap();
    assert!(!page.contains(&secret.to_string()));
    let page = Html::parse_document(&page);
    let scripts = page
        .select(&Selector::parse("script[src]").unwrap())
        .filter_map(|script| script.value().attr("src"))
        .collect::<Vec<_>>();
    assert_eq!(
        scripts,
        [
            "/assets/libs/alpinejs-persist@3.14.3.js",
            "/assets/libs/alpinejs-intersect@3.14.3.js",
            "/assets/libs/alpinejs-morph@3.14.3.js",
            "/assets/libs/alpine-ajax@0.12.6.js",
            "/assets/app.js",
            "/assets/libs/alpinejs@3.14.3.js",
            "/assets/libs/prismjs/prism-core.min.js",
            "/assets/libs/prismjs/prism-c.min.js",
            "/assets/libs/prismjs/prism-cpp.min.js",
            "/assets/libs/prismjs/prism-javascript.min.js",
            "/assets/libs/prismjs/prism-python.min.js",
            "/assets/libs/prismjs/prism-rust.min.js",
            "/assets/libs/prismjs/prism-java.min.js",
            "/assets/libs/prismjs/prism-bash.min.js",
            "/assets/libs/prismjs/prism-json.min.js",
            "/assets/libs/prismjs/prism-yaml.min.js",
            "/assets/libs/prismjs/prism-markdown.min.js",
            "/assets/libs/prismjs/prism-sql.min.js",
            "/assets/libs/prismjs/prism-toolbar.min.js",
            "/assets/libs/prismjs/prism-copy-to-clipboard.min.js",
            "/assets/libs/mathjax-3.2.2/tex-mml-chtml.js",
            "/assets/emoji-init.js"
        ]
    );
    let response = writer.get("/messages/invalid").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    private_headers(&response);
    assert!(response.text().await.unwrap().contains("<html"));
    let response = writer.get("/messages/no/such/page").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    private_headers(&response);
    let csrf = settings_token(&writer).await;
    let before = server
        .client(id)
        .await
        .db()
        .dm_local_installation()
        .await
        .unwrap()
        .unwrap()
        .device_id;
    let response = observer
        .post_form(
            "/settings/messages",
            &[
                ("csrf", csrf.as_str()),
                ("action", "retire"),
                ("device", &data_encoding::HEXLOWER.encode(&before)),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    private_headers(&response);
    let other_writer = server.driver();
    other_writer
        .login_with_secret(&id.to_string(), &secret.to_string())
        .await;
    let other_csrf = settings_token(&other_writer).await;
    assert_ne!(csrf, other_csrf);
    let response = writer
        .post_form(
            "/settings/messages",
            &[
                ("csrf", other_csrf.as_str()),
                ("action", "retire"),
                ("device", &data_encoding::HEXLOWER.encode(&before)),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        !server
            .client(id)
            .await
            .db()
            .dm_local_installation()
            .await
            .unwrap()
            .unwrap()
            .retired
    );
    writer.post_form("/unlock/logout", &[]).await;
    let response = writer.get("/messages").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/unlock?redirect=%2Fmessages"
    );
    private_headers(&response);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ajax_send_clears_only_the_submitted_draft_instance() {
    let server = TestServer::start().await;
    let alice = server.driver();
    let bob = server.driver();
    let (alice_id, alice_secret) = alice.login_new_identity().await;
    let (bob_id, _) = bob.login_new_identity().await;
    server
        .client(alice_id)
        .await
        .post_social_profile_update(
            alice_secret,
            "Alice Example".to_owned(),
            String::new(),
            None,
        )
        .await
        .unwrap();
    replicate_device(&server, bob_id, alice_id).await;

    let path = format!("/messages/{}", bob_id.to_short());
    let page = alice.get(&path).await;
    assert_eq!(page.status(), StatusCode::OK);
    let csrf = token(&page.text().await.unwrap());
    let sent_draft_token = "a".repeat(64);
    let response = alice
        .ajax_post_form(
            &path,
            &[
                ("text", "sent with Alpine"),
                ("csrf", csrf.as_str()),
                ("draft_token", sent_draft_token.as_str()),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let page = Html::parse_document(&response.text().await.unwrap());
    assert!(
        page.select(&Selector::parse("#direct-message-thread").unwrap())
            .next()
            .is_some()
    );
    assert_eq!(
        page.select(&Selector::parse(".m-directMessages__message.-outgoing strong").unwrap())
            .next()
            .expect("enhanced outgoing author")
            .text()
            .collect::<String>(),
        "Alice Example"
    );
    let composer = page
        .select(&Selector::parse("form[x-init]").unwrap())
        .next()
        .expect("successful AJAX response draft clear");
    let clear = composer.value().attr("x-init").unwrap();
    assert!(clear.contains(&format!("draftToken === \"{sent_draft_token}\"")));
    assert!(clear.contains(&format!("direct-message-draft-token-{alice_id}-{bob_id}")));
    assert!(clear.contains("localStorage.getItem"));
    assert!(clear.contains("text = ''"));
    assert!(clear.contains("draftToken = "));
    let textarea = page
        .select(&Selector::parse("textarea[name=text]").unwrap())
        .next()
        .expect("AJAX response composer");
    assert_eq!(textarea.value().attr("autofocus"), None);
    assert_eq!(textarea.value().attr("x-init"), None);

    let page = alice.get(&path).await;
    assert_eq!(page.status(), StatusCode::OK);
    let page = Html::parse_document(&page.text().await.unwrap());
    assert!(
        page.select(&Selector::parse("form[x-init]").unwrap())
            .next()
            .is_none(),
        "ordinary navigation must not clear a draft"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_http_send_receive_retirement_and_reenrollment() {
    let server = TestServer::start().await;
    let alice = server.driver();
    let bob = server.driver();
    let (alice_id, alice_secret) = alice.login_new_identity().await;
    let (bob_id, bob_secret) = bob.login_new_identity().await;
    server
        .client(alice_id)
        .await
        .post_social_profile_update(
            alice_secret,
            "Alice Example".to_owned(),
            String::new(),
            None,
        )
        .await
        .unwrap();
    let alice_profile_event = server
        .client(alice_id)
        .await
        .db()
        .get_social_profile(alice_id)
        .await
        .unwrap()
        .event_id;
    server
        .client(bob_id)
        .await
        .post_social_profile_update(bob_secret, "Bob Example".to_owned(), String::new(), None)
        .await
        .unwrap();
    let bob_profile_event = server
        .client(bob_id)
        .await
        .db()
        .get_social_profile(bob_id)
        .await
        .unwrap()
        .event_id;
    replicate_device(&server, alice_id, bob_id).await;
    replicate_device(&server, bob_id, alice_id).await;
    replicate_event(&server, alice_id, bob_id, alice_profile_event).await;
    replicate_event(&server, bob_id, alice_id, bob_profile_event).await;
    let path = format!("/messages/{}", bob_id.to_short());
    let page = alice.get("/messages").await.text().await.unwrap();
    let document = Html::parse_document(&page);
    assert_conversation_panel_keeps_start_form(&document);
    let open_form = document
        .select(&Selector::parse("form[action='/messages/open']").unwrap())
        .next()
        .expect("start-conversation form");
    assert_eq!(open_form.value().attr("method"), Some("get"));
    let peer_input = open_form
        .select(&Selector::parse("input[name='peer']").unwrap())
        .next()
        .expect("recipient input");
    let peer_label = open_form
        .select(&Selector::parse("label[for='message-peer']").unwrap())
        .next()
        .expect("recipient input label");
    assert_eq!(
        peer_label.value().attr("class"),
        Some("m-directMessages__startLabel")
    );
    assert_eq!(
        peer_label.text().collect::<String>(),
        "Start a conversation"
    );
    let stylesheet = include_str!("../assets/style.css");
    assert!(stylesheet.contains(
        ".m-directMessages__startLabel {\n  position: absolute;\n  width: 1px;\n  height: 1px;"
    ));
    assert!(
        stylesheet
            .contains(".m-directMessages__startRow input {\n  flex: 1 1 auto;\n  min-width: 0;")
    );
    assert_eq!(
        peer_input.value().attr("list"),
        Some("message-peer-suggestions")
    );
    let bob_short = bob_id.to_short().to_string();
    let suggestion = open_form
        .select(&Selector::parse("datalist option").unwrap())
        .find(|option| option.value().attr("value") == Some(bob_short.as_str()))
        .expect("locally known recipient suggestion");
    assert_eq!(suggestion.value().attr("label"), Some("Bob Example"));
    let response = alice.get(&format!("/messages/open?peer={bob_id}")).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], path);
    private_headers(&response);
    for peer in [
        "invalid".to_owned(),
        rostra_core::id::RostraIdSecretKey::generate()
            .id()
            .to_string(),
    ] {
        let response = alice.get(&format!("/messages/open?peer={peer}")).await;
        assert!(matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
        ));
        private_headers(&response);
        assert!(response.text().await.unwrap().contains("<html"));
    }
    let response = alice
        .get(&format!("/messages/{bob_id}?before_time=1"))
        .await;
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        response.headers()[header::LOCATION],
        format!("{path}?before_time=1")
    );
    private_headers(&response);
    let response = alice.get(&path).await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = response.text().await.unwrap();
    let document = Html::parse_document(&page);
    assert_conversation_panel_keeps_start_form(&document);
    let header = document
        .select(&Selector::parse(".m-directMessages__threadHeader").unwrap())
        .next()
        .expect("conversation header");
    assert_eq!(
        header
            .select(&Selector::parse("h1").unwrap())
            .next()
            .expect("recipient name")
            .text()
            .collect::<String>(),
        "Bob Example"
    );
    let recipient_link = header
        .select(&Selector::parse("h1 a").unwrap())
        .next()
        .expect("recipient profile link");
    assert_eq!(
        recipient_link.value().attr("href"),
        Some(format!("/profile/{}", bob_id.to_short()).as_str())
    );
    assert!(
        !header
            .text()
            .collect::<String>()
            .contains("Private conversation")
    );
    assert!(
        document
            .select(&Selector::parse(".m-directMessages__availabilityWarning").unwrap())
            .next()
            .is_none()
    );
    assert!(
        document
            .select(&Selector::parse("textarea[name=text]").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("disabled")
            .is_none()
    );
    assert_eq!(
        document
            .select(&Selector::parse("textarea[name=text]").unwrap())
            .next()
            .expect("message composer")
            .value()
            .attr("aria-label"),
        Some("Message")
    );
    let textarea = document
        .select(&Selector::parse("textarea[name=text]").unwrap())
        .next()
        .expect("message composer");
    assert_eq!(textarea.value().attr("autofocus"), Some(""));
    assert_eq!(
        textarea.value().attr("x-init"),
        Some(
            "$nextTick(() => { document.body.scrollTo(0, document.body.scrollHeight); $el.focus(); })"
        )
    );
    let composer = document
        .select(&Selector::parse("form[action^='/messages/']").unwrap())
        .find(|form| {
            form.select(&Selector::parse("textarea[name=text]").unwrap())
                .next()
                .is_some()
        })
        .expect("message composer form");
    let draft_state = composer
        .value()
        .attr("x-data")
        .expect("persisted draft state");
    assert!(draft_state.contains(&alice_id.to_string()));
    assert!(draft_state.contains(&bob_id.to_string()));
    for target in composer
        .value()
        .attr("x-target")
        .unwrap()
        .split_whitespace()
    {
        assert!(
            document
                .select(&Selector::parse(&format!("#{target}")).unwrap())
                .next()
                .is_some()
        );
    }
    assert!(
        composer
            .value()
            .attr("x-target")
            .unwrap()
            .contains("direct-message-conversations")
    );
    assert!(composer.value().attr("@ajax:before").is_some());
    assert!(composer.value().attr("@ajax:after").is_some());
    assert_eq!(
        composer.value().attr("x-on:keyup.enter.ctrl"),
        Some(
            "if (!$event.repeat && !$event.isComposing && $event.keyCode !== 229) { $el.requestSubmit(); }"
        )
    );
    assert_eq!(
        composer
            .select(&Selector::parse("textarea[name=text]").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("x-model"),
        Some("text")
    );
    assert_eq!(
        composer
            .select(&Selector::parse("textarea[name=text]").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("@input"),
        Some(
            "handleInput($event); draftToken = Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, '0')).join('')"
        )
    );
    let textarea_wrapper = composer
        .select(&Selector::parse(".m-directMessages__textareaWrapper").unwrap())
        .next()
        .expect("autocomplete textarea wrapper");
    let autocomplete_state = textarea_wrapper
        .value()
        .attr("x-data")
        .expect("autocomplete state");
    assert!(autocomplete_state.starts_with("textAutocomplete({"));
    assert!(autocomplete_state.contains("profileSearchUrl: '/messages/profile-search'"));
    assert!(autocomplete_state.contains("profileSearchCsrf:"));
    assert_eq!(
        textarea.value().attr("@keydown"),
        Some("handleKeydown($event)")
    );
    assert_eq!(textarea.value().attr("role"), Some("combobox"));
    assert_eq!(textarea.value().attr("aria-autocomplete"), Some("list"));
    assert_eq!(
        textarea.value().attr("aria-controls"),
        Some("message-text-autocomplete")
    );
    assert_eq!(
        textarea.value().attr(":aria-expanded"),
        Some("showDropdown")
    );
    assert!(
        textarea
            .value()
            .attr(":aria-activedescendant")
            .is_some_and(|value| value.contains("message-text-autocomplete-option-"))
    );
    let autocomplete = textarea_wrapper
        .select(&Selector::parse("#message-text-autocomplete").unwrap())
        .next()
        .expect("autocomplete listbox");
    assert_eq!(autocomplete.value().attr("role"), Some("listbox"));
    let option = autocomplete
        .select(&Selector::parse("[role=option]").unwrap())
        .next()
        .expect("autocomplete option template");
    assert_eq!(
        option.value().attr(":aria-selected"),
        Some("index === selectedIndex")
    );
    let emoji_button = composer
        .select(&Selector::parse(".m-directMessages__emojiButton").unwrap())
        .next()
        .expect("emoji picker button");
    assert_eq!(
        emoji_button.value().attr("aria-label"),
        Some("Insert emoji")
    );
    assert_eq!(
        emoji_button.value().attr("@click"),
        Some("toggleEmojiPicker('message-emoji-picker', $event)")
    );
    assert_eq!(emoji_button.value().attr("onclick"), None);
    let emoji_picker = composer
        .select(&Selector::parse("#message-emoji-picker").unwrap())
        .next()
        .expect("emoji picker");
    assert_eq!(
        emoji_picker.value().attr("data-textarea-selector"),
        Some("#message-text")
    );
    assert!(
        emoji_picker
            .select(&Selector::parse("emoji-picker").unwrap())
            .next()
            .is_some()
    );
    assert!(
        document
            .select(&Selector::parse(".m-directMessages__sendButton").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("disabled")
            .is_none()
    );
    assert_eq!(
        document
            .select(&Selector::parse(".m-directMessages__sendButton").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("title"),
        Some("Send message (Ctrl+Enter)")
    );
    let csrf = token(&page);
    let response = alice
        .post_form(
            "/messages/profile-search",
            &[("q", "Alice"), ("csrf", "wrong")],
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    private_headers(&response);
    let response = alice
        .post_form(
            "/messages/profile-search",
            &[("q", "Alice"), ("csrf", csrf.as_str())],
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    assert!(
        response
            .json::<serde_json::Value>()
            .await
            .unwrap()
            .as_array()
            .is_some_and(|results| !results.is_empty())
    );
    let text = "# Rendered message\n\n*important* and $`x^2`\n\n<img src=\"https://outsider.invalid/pixel\">\n<style>body { display: none }</style>\n<span style=\"background: red\">styled</span>\n\n[unsafe](javascript:alert(1))";
    let response = alice
        .post_form(&path, &[("text", text), ("csrf", "wrong")])
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        server
            .client(alice_id)
            .await
            .db()
            .dm_history_with(bob_id, None, 64)
            .await
            .unwrap()
            .is_empty()
    );
    let oversized = "é".repeat(8193);
    let response = alice
        .post_form(
            &path,
            &[("text", oversized.as_str()), ("csrf", csrf.as_str())],
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    private_headers(&response);
    let document = Html::parse_document(&response.text().await.unwrap());
    let textarea = document
        .select(&Selector::parse("textarea").unwrap())
        .next()
        .expect("validation response composer");
    assert_eq!(textarea.text().collect::<String>(), oversized);
    assert_eq!(textarea.value().attr("autofocus"), None);
    assert_eq!(textarea.value().attr("x-init"), None);
    let response = alice
        .post_form(&path, &[("text", text), ("csrf", csrf.as_str())])
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], path);
    private_headers(&response);
    let alice_client = server.client(alice_id).await;
    let bob_client = server.client(bob_id).await;
    let sent = alice_client
        .db()
        .dm_history_with(bob_id, None, 64)
        .await
        .unwrap();
    assert_eq!(sent.len(), 1);
    let event_id = sent[0].event_id;
    let event = VerifiedEvent::verify_signed(
        alice_id,
        alice_client.db().get_event(event_id).await.unwrap().signed,
    )
    .unwrap();
    let content = alice_client.db().get_event_content(event_id).await.unwrap();
    bob_client
        .db()
        .try_process_event_with_content(&VerifiedEventContent::verify(event, content).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = bob_client
                .db()
                .dm_history_with(alice_id, None, 64)
                .await
                .unwrap();
            if !history.is_empty() {
                assert_eq!(history[0].text, text);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let bob_path = format!("/messages/{}", alice_id.to_short());
    let bob_page = bob.get(&bob_path).await;
    assert_eq!(bob_page.status(), StatusCode::OK);
    let bob_page = Html::parse_document(&bob_page.text().await.unwrap());
    assert_eq!(
        bob_page
            .select(&Selector::parse(".m-directMessages__message:not(.-outgoing) strong").unwrap())
            .next()
            .expect("incoming author")
            .text()
            .collect::<String>(),
        "Alice Example"
    );
    let bob_draft_state = bob_page
        .select(&Selector::parse("form[x-data]").unwrap())
        .next()
        .expect("Bob composer")
        .value()
        .attr("x-data")
        .expect("Bob draft state");
    assert!(bob_draft_state.contains(&bob_id.to_string()));
    assert!(bob_draft_state.contains(&alice_id.to_string()));
    assert_ne!(
        draft_state, bob_draft_state,
        "draft keys must distinguish the current account"
    );
    for (driver, path) in [(&alice, path.clone()), (&bob, bob_path)] {
        let response = driver.get(&path).await;
        private_headers(&response);
        let document = Html::parse_document(&response.text().await.unwrap());
        assert!(
            document
                .select(
                    &Selector::parse("img, iframe, script:not([src]), style, [style]").unwrap(),
                )
                .next()
                .is_none()
        );
        let rendered = document
            .select(&Selector::parse(".m-directMessages__text.m-postView__content").unwrap())
            .find(|element| {
                element
                    .select(&Selector::parse("h1").unwrap())
                    .any(|heading| heading.text().collect::<String>() == "Rendered message")
            })
            .expect("Djot-rendered direct message");
        assert_eq!(
            rendered
                .select(&Selector::parse("strong").unwrap())
                .next()
                .expect("Djot strong text")
                .text()
                .collect::<String>(),
            "important"
        );
        assert_eq!(
            rendered
                .select(&Selector::parse(".math.inline").unwrap())
                .next()
                .expect("Djot inline math")
                .text()
                .collect::<String>(),
            "\\(x^2\\)"
        );
        assert!(
            rendered
                .select(&Selector::parse("a[href='#']").unwrap())
                .any(|link| link.text().collect::<String>() == "unsafe"),
            "dangerous URLs should use the posts' sanitized fallback"
        );
    }
    let page = alice.get(&path).await;
    let page = Html::parse_document(&page.text().await.unwrap());
    assert_eq!(
        page.select(&Selector::parse(".m-directMessages__message.-outgoing strong").unwrap())
            .next()
            .expect("outgoing author")
            .text()
            .collect::<String>(),
        "Alice Example"
    );
    let conversations = alice.get("/messages").await.text().await.unwrap();
    let conversations = Html::parse_document(&conversations);
    let conversation = conversations
        .select(&Selector::parse(&format!("a[href='{path}']")).unwrap())
        .next()
        .expect("conversation");
    assert_eq!(
        conversation
            .select(&Selector::parse(".m-directMessages__conversationPeer").unwrap())
            .next()
            .expect("conversation display name")
            .text()
            .collect::<String>(),
        "Bob Example"
    );
    assert!(
        !conversation.text().collect::<String>().contains(text),
        "conversation list must not expose message previews"
    );
    let selected = alice.get(&path).await;
    let selected = Html::parse_document(&selected.text().await.unwrap());
    let conversation_panel = selected
        .select(&Selector::parse(".m-directMessages__conversationPanel").unwrap())
        .next()
        .expect("conversation panel");
    assert!(
        conversation_panel
            .select(&Selector::parse(&format!("a[aria-current=page][href='{path}']")).unwrap(),)
            .next()
            .is_some(),
        "the open conversation should expose its current-page state"
    );
    let local = alice_client
        .db()
        .dm_local_installation()
        .await
        .unwrap()
        .unwrap()
        .device_id;
    let settings_csrf = settings_token(&alice).await;
    let before = alice_client.db().get_self_current_head().await;
    let confirmation = alice
        .get(&format!(
            "/settings/messages/retire/{}",
            data_encoding::HEXLOWER.encode(&local)
        ))
        .await;
    assert_eq!(confirmation.status(), StatusCode::OK);
    private_headers(&confirmation);
    let confirmation = Html::parse_document(&confirmation.text().await.unwrap());
    assert!(
        confirmation
            .root_element()
            .text()
            .collect::<String>()
            .contains("permanent")
    );
    assert!(
        confirmation
            .select(&Selector::parse("a[href='/settings/messages']").unwrap())
            .any(|link| link.text().collect::<String>() == "Cancel")
    );
    assert_eq!(alice_client.db().get_self_current_head().await, before);
    assert!(
        !alice_client
            .db()
            .dm_local_installation()
            .await
            .unwrap()
            .unwrap()
            .retired
    );
    for fields in [
        vec![("csrf", settings_csrf.as_str()), ("action", "unknown")],
        vec![("csrf", settings_csrf.as_str()), ("action", "retire")],
        vec![
            ("csrf", settings_csrf.as_str()),
            ("action", "retire"),
            ("device", "bad"),
        ],
        vec![
            ("csrf", settings_csrf.as_str()),
            ("action", "reenroll"),
            ("device", "unexpected"),
        ],
    ] {
        let response = alice.post_form("/settings/messages", &fields).await;
        assert!(response.status().is_client_error());
        private_headers(&response);
        assert!(response.text().await.unwrap().contains("<html"));
        assert_eq!(alice_client.db().get_self_current_head().await, before);
        let state = alice_client
            .db()
            .dm_local_installation()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.device_id, local);
        assert!(!state.retired);
    }
    let response = alice
        .post_form(
            "/settings/messages",
            &[
                ("csrf", settings_csrf.as_str()),
                ("action", "retire"),
                ("device", &data_encoding::HEXLOWER.encode(&local)),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        alice_client
            .db()
            .dm_local_installation()
            .await
            .unwrap()
            .unwrap()
            .retired
    );
    let response = alice
        .post_form(&path, &[("text", "not queued"), ("csrf", csrf.as_str())])
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let document = Html::parse_document(&response.text().await.unwrap());
    assert!(
        document
            .select(
                &Selector::parse(".m-directMessages__availabilityWarning[role=status]").unwrap()
            )
            .any(|warning| warning
                .text()
                .collect::<String>()
                .contains("No message will be queued."))
    );
    assert!(
        document
            .select(&Selector::parse("textarea[name=text][disabled]").unwrap())
            .any(|textarea| textarea.text().collect::<String>() == "not queued")
    );
    assert!(
        document
            .select(&Selector::parse(".m-directMessages__sendButton[disabled]").unwrap())
            .next()
            .is_some()
    );
    assert_eq!(
        alice_client
            .db()
            .dm_history_with(bob_id, None, 64)
            .await
            .unwrap()
            .len(),
        1
    );
    let response = alice
        .post_form(
            "/settings/messages",
            &[("csrf", settings_csrf.as_str()), ("action", "reenroll")],
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let replacement = alice_client
        .db()
        .dm_local_installation()
        .await
        .unwrap()
        .unwrap();
    assert_ne!(replacement.device_id, local);
    assert!(!replacement.retired);
    assert_eq!(
        alice_client
            .db()
            .dm_history_with(bob_id, None, 64)
            .await
            .unwrap()
            .len(),
        1
    );
    for index in 1..100 {
        alice_client
            .send_direct_message(alice_secret, bob_id, format!("page {index}"))
            .await
            .unwrap();
    }
    let response = alice.get(&path).await;
    let document = Html::parse_document(&response.text().await.unwrap());
    let history_controller = document
        .select(&Selector::parse("[x-data=directMessageHistory]").unwrap())
        .next()
        .expect("incremental history controller");
    assert_eq!(
        history_controller.value().attr("@ajax:merge"),
        Some("beforeOlderMerge($event)")
    );
    assert_eq!(
        document
            .select(&Selector::parse(".m-directMessages__message").unwrap())
            .count(),
        100
    );
    assert!(
        !document
            .select(&Selector::parse("a").unwrap())
            .any(|link| link.text().collect::<String>() == "Older messages")
    );
    alice_client
        .send_direct_message(alice_secret, bob_id, "one more".to_owned())
        .await
        .unwrap();
    let response = alice.get(&path).await;
    let document = Html::parse_document(&response.text().await.unwrap());
    assert_eq!(
        document
            .select(&Selector::parse(".m-directMessages__message").unwrap())
            .count(),
        100
    );
    let older_link = document
        .select(&Selector::parse("a").unwrap())
        .find(|link| link.text().collect::<String>() == "Older messages")
        .expect("older-history link");
    assert_eq!(
        older_link.value().attr("x-target"),
        Some("direct-message-history-pagination direct-message-history")
    );
    assert_eq!(
        older_link.value().attr("@click.prevent"),
        Some("loadOlder($el, $ajax)")
    );
    let intersection = older_link
        .value()
        .attr("x-init")
        .expect("older-history intersection observer");
    assert!(intersection.contains("IntersectionObserver"));
    assert!(intersection.contains("root.clientHeight * 2.5"));
    assert!(intersection.contains("rootMargin: `${margin}px 0px 0px 0px`"));
    let older = older_link.value().attr("href").expect("older-history URL");
    assert!(older.contains("before_time="));
    assert!(older.contains("&before_event="));

    let response = alice.ajax_get(older).await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let fragment = Html::parse_fragment(&response.text().await.unwrap());
    let pagination = fragment
        .select(&Selector::parse("#direct-message-history-pagination").unwrap())
        .next()
        .expect("older-history pagination fragment");
    assert_eq!(pagination.value().attr("x-merge"), Some("replace"));
    assert!(
        pagination
            .select(&Selector::parse("a").unwrap())
            .next()
            .is_none(),
        "terminal pagination fragments remove the older sentinel"
    );
    let history = fragment
        .select(&Selector::parse("#direct-message-history").unwrap())
        .next()
        .expect("older-history row fragment");
    assert_eq!(
        history.value().attr("x-merge:dynamic"),
        Some("loadingNew ? 'append' : 'prepend'")
    );
    assert_eq!(
        history
            .select(&Selector::parse(".m-directMessages__message").unwrap())
            .count(),
        1
    );
    assert!(
        history
            .select(&Selector::parse(".m-directMessages__message[id]").unwrap())
            .all(|message| message
                .value()
                .attr("id")
                .is_some_and(|id| id.starts_with("direct-message-")))
    );
    assert!(
        fragment
            .select(&Selector::parse("#direct-message-thread").unwrap())
            .next()
            .is_none(),
        "pagination fragments must contain only bounded history targets"
    );
    assert!(
        fragment
            .select(&Selector::parse("textarea[name=text]").unwrap())
            .next()
            .is_none()
    );

    let response = alice.get(older).await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let document = Html::parse_document(&response.text().await.unwrap());
    assert_eq!(
        document
            .select(&Selector::parse(".m-directMessages__message").unwrap())
            .count(),
        1
    );
    assert!(
        !document
            .select(&Selector::parse("a").unwrap())
            .any(|link| link.text().collect::<String>() == "Older messages")
    );
    let textarea = document
        .select(&Selector::parse("textarea[name=text]").unwrap())
        .next()
        .expect("older-history composer");
    assert_eq!(textarea.value().attr("autofocus"), None);
    assert_eq!(textarea.value().attr("x-init"), None);

    for index in 101..201 {
        alice_client
            .send_direct_message(alice_secret, bob_id, format!("page {index}"))
            .await
            .unwrap();
    }
    let response = alice.get(&path).await;
    let latest = Html::parse_document(&response.text().await.unwrap());
    let first_older = latest
        .select(&Selector::parse(".m-directMessages__older").unwrap())
        .next()
        .expect("first older-history link")
        .value()
        .attr("href")
        .unwrap()
        .to_owned();
    let response = alice.ajax_get(&first_older).await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let first_older = Html::parse_fragment(&response.text().await.unwrap());
    let second_older = first_older
        .select(&Selector::parse(".m-directMessages__older").unwrap())
        .next()
        .expect("second older-history link")
        .value()
        .attr("href")
        .unwrap()
        .to_owned();
    let response = alice.ajax_get(&second_older).await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let second_older = Html::parse_fragment(&response.text().await.unwrap());
    let message_selector = Selector::parse(".m-directMessages__message").unwrap();
    assert_eq!(latest.select(&message_selector).count(), 100);
    assert_eq!(first_older.select(&message_selector).count(), 100);
    assert_eq!(second_older.select(&message_selector).count(), 1);
    assert!(
        second_older
            .select(&Selector::parse(".m-directMessages__older").unwrap())
            .next()
            .is_none()
    );
    let ids = latest
        .select(&message_selector)
        .chain(first_older.select(&message_selector))
        .chain(second_older.select(&message_selector))
        .map(|message| message.value().attr("id").unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 201);

    for invalid_cursor in [
        format!("{path}?before_time=1"),
        format!(
            "{path}?before_event={}",
            sent.first().expect("sent message").event_id
        ),
    ] {
        let response = alice.get(&invalid_cursor).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        private_headers(&response);
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("both cursor fields")
        );
    }
    drop(alice_client);
    drop(bob_client);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_conversation_fragments_append_incoming_and_keep_session_authority() {
    let server = TestServer::start().await;
    let alice = server.driver();
    let bob = server.driver();
    let (alice_id, _) = alice.login_new_identity().await;
    let (bob_id, bob_secret) = bob.login_new_identity().await;
    replicate_device(&server, alice_id, bob_id).await;
    replicate_device(&server, bob_id, alice_id).await;
    let path = format!("/messages/{}", bob_id.to_short());
    let initial = alice.get(&path).await;
    private_headers(&initial);
    let initial = Html::parse_document(&initial.text().await.unwrap());
    assert_eq!(
        initial
            .select(&Selector::parse("[x-data=\"websocket('/updates')\"]").unwrap())
            .count(),
        1,
        "the open conversation owns one existing UI update connection"
    );
    let cursor_selector = Selector::parse("#direct-message-live-cursor").unwrap();
    let initial_cursor = initial
        .select(&cursor_selector)
        .next()
        .unwrap()
        .value()
        .attr("data-after-sequence")
        .unwrap();
    assert_eq!(initial_cursor, "0");
    let mut updates = alice.updates_websocket().await;
    let initial_wakeup = tokio::time::timeout(Duration::from_secs(10), updates.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert!(initial_wakeup.contains("$dispatch('messages:changed')"));
    let bob_client = server.client(bob_id).await;
    let alice_client = server.client(alice_id).await;
    for text in ["first incoming", "second incoming"] {
        let event = bob_client
            .send_direct_message(bob_secret, alice_id, text.to_owned())
            .await
            .unwrap();
        replicate_event(&server, bob_id, alice_id, event.event_id.to_short()).await;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while alice_client
            .db()
            .dm_latest_incoming_sequence(bob_id)
            .await
            .unwrap()
            != 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = updates.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(
                !frame.contains("incoming"),
                "websocket wakeups carry no plaintext"
            );
            if frame.contains("$dispatch('messages:changed')") {
                break;
            }
        }
    })
    .await
    .unwrap();
    let live_url = format!("{path}?after_sequence={initial_cursor}");
    let response = alice.ajax_get(&live_url).await;
    assert_eq!(response.status(), StatusCode::OK);
    private_headers(&response);
    let fragment = Html::parse_fragment(&response.text().await.unwrap());
    let messages = Selector::parse(".m-directMessages__message").unwrap();
    assert_eq!(fragment.select(&messages).count(), 2);
    assert_eq!(
        fragment
            .select(&cursor_selector)
            .next()
            .unwrap()
            .value()
            .attr("data-after-sequence"),
        Some("2")
    );
    assert!(
        fragment
            .select(&Selector::parse("form, textarea, #direct-message-history-pagination").unwrap())
            .next()
            .is_none()
    );
    let response = alice.ajax_get(&live_url).await;
    private_headers(&response);
    let repeated = Html::parse_fragment(&response.text().await.unwrap());
    assert_eq!(
        repeated.select(&messages).count(),
        2,
        "read markers do not replace the arrival cursor"
    );
    let response = alice.ajax_get(&format!("{path}?after_sequence=2")).await;
    private_headers(&response);
    let empty = Html::parse_fragment(&response.text().await.unwrap());
    assert_eq!(empty.select(&messages).count(), 0);
    let response = alice.get(&live_url).await;
    private_headers(&response);
    let ordinary = Html::parse_document(&response.text().await.unwrap());
    assert!(
        ordinary
            .select(&Selector::parse("form[method=post] textarea").unwrap())
            .next()
            .is_some()
    );
    let observer = server.driver();
    observer.login_readonly(alice_id).await;
    let mut observer_updates = observer.updates_websocket().await;
    let frame = tokio::time::timeout(Duration::from_secs(10), observer_updates.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert!(
        !frame.contains("messages:changed"),
        "read-only sessions do not receive DM wakeups"
    );
    let response = observer.ajax_get(&live_url).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    private_headers(&response);
    assert!(!response.text().await.unwrap().contains("first incoming"));
    alice.post_form("/unlock/logout", &[]).await;
    let response = alice.ajax_get(&live_url).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    private_headers(&response);
    drop(updates);
    drop(observer_updates);
    drop(alice_client);
    drop(bob_client);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_header_uses_unnamed_fallback_and_escapes_profile_names() {
    let server = TestServer::start().await;
    let alice = server.driver();
    let bob = server.driver();
    let charlie = server.driver();
    let (alice_id, _) = alice.login_new_identity().await;
    let (bob_id, bob_secret) = bob.login_new_identity().await;
    let (charlie_id, charlie_secret) = charlie.login_new_identity().await;
    replicate_device(&server, bob_id, alice_id).await;
    replicate_device(&server, charlie_id, alice_id).await;
    replicate_device(&server, alice_id, charlie_id).await;

    let fallback = alice
        .get(&format!("/messages/{}", charlie_id.to_short()))
        .await;
    assert_eq!(fallback.status(), StatusCode::OK);
    private_headers(&fallback);
    let fallback = Html::parse_document(&fallback.text().await.unwrap());
    let fallback_header = fallback
        .select(&Selector::parse(".m-directMessages__threadHeader").unwrap())
        .next()
        .expect("conversation header");
    assert_eq!(
        fallback_header
            .select(&Selector::parse("h1").unwrap())
            .next()
            .expect("fallback identity")
            .text()
            .collect::<String>(),
        "Unnamed profile"
    );
    assert_eq!(
        fallback_header
            .select(&Selector::parse("h1 a").unwrap())
            .next()
            .expect("fallback profile link")
            .value()
            .attr("href"),
        Some(format!("/profile/{}", charlie_id.to_short()).as_str())
    );
    let charlie_draft = fallback
        .select(&Selector::parse("form[x-data]").unwrap())
        .next()
        .expect("Charlie composer")
        .value()
        .attr("x-data")
        .expect("Charlie draft key")
        .to_owned();
    assert!(charlie_draft.contains(&alice_id.to_string()));
    assert!(charlie_draft.contains(&charlie_id.to_string()));
    server
        .client(charlie_id)
        .await
        .send_direct_message(charlie_secret, alice_id, "from unnamed profile".to_owned())
        .await
        .unwrap();
    let event_id = server
        .client(charlie_id)
        .await
        .db()
        .dm_history_with(alice_id, None, 1)
        .await
        .unwrap()
        .pop()
        .expect("sent message")
        .event_id;
    replicate_event(&server, charlie_id, alice_id, event_id).await;
    let alice_client = server.client(alice_id).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !alice_client
                .db()
                .dm_history_with(charlie_id, None, 1)
                .await
                .unwrap()
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("incoming message should reach Alice's history");
    let fallback = alice
        .get(&format!("/messages/{}", charlie_id.to_short()))
        .await;
    assert_eq!(fallback.status(), StatusCode::OK);
    private_headers(&fallback);
    let fallback = Html::parse_document(&fallback.text().await.unwrap());
    assert_eq!(
        fallback
            .select(&Selector::parse(".m-directMessages__message:not(.-outgoing) strong").unwrap())
            .next()
            .expect("unnamed incoming author")
            .text()
            .collect::<String>(),
        "Unnamed profile"
    );
    drop(alice_client);

    let malicious_name = "<img src=x onerror=alert(1)>".to_owned();
    server
        .client(bob_id)
        .await
        .post_social_profile_update(bob_secret, malicious_name.clone(), String::new(), None)
        .await
        .unwrap();
    let profile_event = server
        .client(bob_id)
        .await
        .db()
        .get_social_profile(bob_id)
        .await
        .unwrap()
        .event_id;
    replicate_event(&server, bob_id, alice_id, profile_event).await;

    let page = alice.get(&format!("/messages/{}", bob_id.to_short())).await;
    assert_eq!(page.status(), StatusCode::OK);
    private_headers(&page);
    let page = page.text().await.unwrap();
    assert!(!page.contains(&malicious_name));
    let document = Html::parse_document(&page);
    let bob_draft = document
        .select(&Selector::parse("form[x-data]").unwrap())
        .next()
        .expect("Bob composer")
        .value()
        .attr("x-data")
        .expect("Bob draft key");
    assert!(bob_draft.contains(&alice_id.to_string()));
    assert!(bob_draft.contains(&bob_id.to_string()));
    assert_ne!(bob_draft, charlie_draft);
    let header = document
        .select(&Selector::parse(".m-directMessages__threadHeader").unwrap())
        .next()
        .expect("conversation header");
    assert_eq!(
        header
            .select(&Selector::parse("h1").unwrap())
            .next()
            .expect("escaped recipient name")
            .text()
            .collect::<String>(),
        malicious_name
    );
    assert_eq!(
        header
            .select(&Selector::parse("h1 a").unwrap())
            .next()
            .expect("recipient profile link")
            .value()
            .attr("href"),
        Some(format!("/profile/{}", bob_id.to_short()).as_str())
    );
    assert!(
        header
            .select(&Selector::parse("img, script, iframe").unwrap())
            .next()
            .is_none(),
        "profile names must remain escaped text"
    );
    server.shutdown().await;
}
