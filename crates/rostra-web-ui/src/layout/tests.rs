use scraper::{Html, Selector};
use serde_json::json;

use super::PageResources;
use crate::UiState;

#[test]
fn json_ld_is_safe_for_script_data_and_preserves_json_values() {
    let value = json!({
        "adversarial": "</script><ScRiPt>alert(1)</sCrIpT><!-- <script",
        "ordinary": "quotes: \" backslash: \\ ampersand: & unicode: 世界 literal: \\u003c",
    });
    let serialized = value.to_string();
    let rendered = UiState::render_html_head(
        "Title",
        None,
        None,
        Some(&serialized),
        false,
        PageResources::Standard,
    )
    .into_string();
    let document = Html::parse_fragment(&rendered);
    let json_ld_selector = Selector::parse("script[type='application/ld+json']").unwrap();
    let mut json_ld = document.select(&json_ld_selector);
    let payload = json_ld
        .next()
        .expect("JSON-LD script should be present")
        .text()
        .collect::<String>();

    assert!(json_ld.next().is_none());
    assert!(!payload.contains('<'));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap(),
        value
    );
    assert_eq!(
        document
            .select(&Selector::parse("script[src='/assets/app.js']").unwrap())
            .count(),
        1,
        "resources after JSON-LD should remain in the parsed head"
    );
    assert_eq!(
        document
            .select(
                &Selector::parse("script:not([src]):not([type='application/ld+json'])").unwrap()
            )
            .count(),
        0,
        "JSON-LD content must not create another script element"
    );
}

#[test]
fn absent_json_ld_remains_absent() {
    let rendered =
        UiState::render_html_head("Title", None, None, None, false, PageResources::Standard)
            .into_string();
    let document = Html::parse_fragment(&rendered);

    assert_eq!(
        document
            .select(&Selector::parse("script[type='application/ld+json']").unwrap())
            .count(),
        0
    );
}

#[test]
fn viewport_requests_layout_resize_for_interactive_widgets() {
    let rendered =
        UiState::render_html_head("Title", None, None, None, false, PageResources::Standard)
            .into_string();
    let document = Html::parse_fragment(&rendered);
    let viewport = document
        .select(&Selector::parse("meta[name='viewport']").unwrap())
        .next()
        .expect("viewport metadata");

    assert_eq!(
        viewport.value().attr("content"),
        Some("width=device-width, initial-scale=1.0, interactive-widget=resizes-content")
    );
}
#[test]
fn image_dialog_is_named_closed_and_has_no_initial_media_request() {
    let rendered = super::render_post_image_dialog();
    let document = Html::parse_document(&rendered.into_string());
    let dialog = document
        .select(&Selector::parse("dialog").unwrap())
        .next()
        .unwrap();
    assert_eq!(
        dialog.value().attr("aria-label"),
        Some("Enlarged post image")
    );
    assert!(dialog.value().attr("open").is_none());
    let close = dialog
        .select(&Selector::parse("button").unwrap())
        .next()
        .unwrap();
    assert_eq!(close.text().collect::<String>(), "Close");
    assert_eq!(close.value().attr("type"), Some("button"));
    assert!(close.value().classes().any(|class| class == "u-button"));
    assert_eq!(
        close
            .select(&Selector::parse(".o-imageDialog__closeButtonIcon.u-buttonIcon").unwrap())
            .count(),
        1
    );
    let image = dialog
        .select(&Selector::parse("img").unwrap())
        .next()
        .unwrap();
    assert!(image.value().attr("src").is_none());
    for resources in [PageResources::Private, PageResources::PrivateRich] {
        let rendered = super::render_html_body(maud::html! {}, "", resources);
        let document = Html::parse_document(&rendered.into_string());
        assert_eq!(
            document
                .select(&Selector::parse("dialog, img").unwrap())
                .count(),
            0
        );
    }
}
