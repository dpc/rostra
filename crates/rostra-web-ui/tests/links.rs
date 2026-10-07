mod common;

use common::TestServer;
use scraper::{Html, Selector};

fn attr(document: &Html, selector: &str, attribute: &str) -> String {
    document
        .select(&Selector::parse(selector).unwrap())
        .next()
        .unwrap()
        .value()
        .attr(attribute)
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn x_posts_work_through_add_link_and_render_as_explicit_load_widgets() {
    let server = TestServer::start().await;
    let driver = server.driver();
    driver.login_new_identity().await;
    let response = driver
        .post_form(
            "/post/link_dialog",
            &[
                ("url", "https://x.com/alice/status/123?s=20"),
                ("title", "A post"),
                ("action", "insert"),
            ],
        )
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let syntax = document
        .select(&Selector::parse("#link-result").unwrap())
        .next()
        .unwrap()
        .text()
        .collect::<String>();
    assert_eq!(syntax, "![A post](https://x.com/alice/status/123?s=20)");
    for content in [
        syntax.as_str(),
        "[A post](https://twitter.com/alice/status/123)",
        "<https://x.com/alice/status/123>",
    ] {
        let response = driver
            .post_form("/post/preview_dialog", &[("content", content)])
            .await;
        assert_eq!(response.status(), 200);
        let document = Html::parse_document(&response.text().await.unwrap());
        assert_eq!(
            document
                .select(&Selector::parse(".lazyload-wrapper").unwrap())
                .count(),
            1
        );
        assert_eq!(
            attr(&document, "a.lazyload-message", "href"),
            "https://twitter.com/i/web/status/123"
        );
        let iframe = document
            .select(&Selector::parse(".lazyload-wrapper iframe").unwrap())
            .next()
            .unwrap();
        assert!(iframe.value().attr("srcdoc").is_none());
        assert!(iframe.value().attr("src").is_none());
        assert_eq!(
            document
                .select(&Selector::parse("[data-embed-fallback]").unwrap())
                .count(),
            0
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_link_flow_keeps_draft_and_supports_manual_title_after_fetch_failure() {
    let server = TestServer::start().await;
    let driver = server.driver();
    driver.login_new_identity().await;
    let draft = "My existing draft <not markup>";
    let response = driver
        .post_form("/post/link_dialog", &[("content", draft)])
        .await;
    assert_eq!(response.status(), 200);
    let page = response.text().await.unwrap();
    assert!(page.contains("<!DOCTYPE html>"));
    let document = Html::parse_document(&page);
    assert_eq!(
        attr(
            &document,
            "form[action='/post/link_dialog'] input[name=content]",
            "value"
        ),
        draft
    );
    assert_eq!(
        attr(&document, "form[action='/post/link_dialog']", "method"),
        "post"
    );
    let response = driver
        .post_form(
            "/post/link_dialog",
            &[
                ("content", draft),
                ("url", "http://127.0.0.1/"),
                ("action", "title"),
            ],
        )
        .await;
    assert_eq!(response.status(), 422);
    let page = response.text().await.unwrap();
    assert!(page.contains("Could not fetch a title"));
    let failed = Html::parse_document(&page);
    assert_eq!(
        attr(
            &failed,
            "form[action='/post/link_dialog'] input[name=content]",
            "value"
        ),
        draft
    );
    let response = driver
        .post_form(
            "/post/link_dialog",
            &[
                ("content", draft),
                ("url", "http://127.0.0.1/"),
                ("title", "Manual title"),
                ("action", "insert"),
            ],
        )
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_document(&response.text().await.unwrap());
    let textarea = document
        .select(&Selector::parse("#link-result").unwrap())
        .next()
        .unwrap();
    assert_eq!(
        textarea.text().collect::<String>(),
        format!("{draft} [Manual title](http://127.0.0.1/)")
    );
    assert_eq!(
        attr(&document, "form[action='/post/preview_dialog']", "method"),
        "post"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn enhanced_link_flow_returns_escaped_server_owned_syntax_and_cancel_keeps_draft() {
    let server = TestServer::start().await;
    let driver = server.driver();
    driver.login_new_identity().await;
    let response = driver
        .ajax_post_form(
            "/post/link_dialog",
            &[
                ("content", "Draft"),
                ("url", "https://youtu.be/123"),
                ("title", "A title"),
                ("action", "insert"),
            ],
        )
        .await;
    assert_eq!(response.status(), 200);
    let document = Html::parse_fragment(&response.text().await.unwrap());
    assert_eq!(attr(&document, "dialog", "id"), "post-preview-dialog");
    assert_eq!(
        attr(&document, "[data-link-syntax]", "data-link-syntax"),
        "![A title](https://youtu.be/123)"
    );
    let response = driver
        .post_form(
            "/post/link_dialog",
            &[("content", "Draft"), ("action", "cancel")],
        )
        .await;
    let document = Html::parse_document(&response.text().await.unwrap());
    assert_eq!(
        document
            .select(&Selector::parse("#link-result").unwrap())
            .next()
            .unwrap()
            .text()
            .collect::<String>(),
        "Draft"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_title_request_renders_hypermedia_form_error_and_keeps_manual_insert_available() {
    let server = TestServer::start().await;
    let driver = server.driver();
    driver.login_new_identity().await;
    let request_id = "0123456789abcdef0123456789abcdef";
    for url in ["http://127.0.0.1/", "not a URL"] {
        let response = driver
            .ajax_post_form(
                "/post/link_dialog",
                &[
                    ("content", "Retained draft"),
                    ("title", "Retained title"),
                    ("url", url),
                    ("action", "title"),
                    ("link_request", request_id),
                ],
            )
            .await;
        assert_eq!(response.status(), 422);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let document = Html::parse_fragment(&response.text().await.unwrap());
        assert_eq!(attr(&document, "dialog", "data-link-request"), request_id);
        assert_eq!(
            attr(&document, "form", "x-target.error"),
            "post-preview-dialog"
        );
        assert_eq!(
            attr(&document, "input[name=content]", "value"),
            "Retained draft"
        );
        assert_eq!(
            attr(&document, "input[name=title]", "value"),
            "Retained title"
        );
        assert_eq!(attr(&document, "input[name=url]", "value"), url);
        assert!(
            document
                .select(&Selector::parse("#link-form-error[role=alert]").unwrap())
                .next()
                .is_some()
        );
        for action in ["title", "insert"] {
            assert!(
                document
                    .select(
                        &Selector::parse(&format!("button[value={action}]:not([disabled])"))
                            .unwrap()
                    )
                    .next()
                    .is_some()
            );
        }
    }
    let response = driver
        .ajax_post_form(
            "/post/link_dialog",
            &[
                ("content", "Retained draft"),
                ("title", "Retained title"),
                ("url", "http://127.0.0.1/"),
                ("action", "insert"),
                ("link_request", request_id),
            ],
        )
        .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn link_dialog_requires_an_authenticated_signing_session() {
    let server = TestServer::start().await;
    let driver = server.driver();
    let response = driver
        .post_form(
            "/post/link_dialog",
            &[("url", "http://127.0.0.1/"), ("action", "title")],
        )
        .await;
    assert_ne!(response.status(), 200);
}
