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
    assert_eq!(response.status(), 200);
    let page = response.text().await.unwrap();
    assert!(page.contains("Could not fetch a title"));
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
        format!("{draft}\n[Manual title](http://127.0.0.1/)")
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
