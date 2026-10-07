use std::str::FromStr;

use jotup::r#async::AsyncRenderOutputExt;

use super::{RostraRenderExt, make_base_renderer};
use crate::UiState;

mod url_sanitization;
mod xss_sanitization;

/// Valid base32 test event ID (16 bytes = 26 base32 characters)
const TEST_EVENT_ID: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn x_post_recognition_is_strict_and_discards_tracking_parameters() {
    for input in [
        "https://x.com/alice/status/123?s=20#fragment",
        "http://www.x.com/alice/status/123/",
        "https://twitter.com/alice/status/123/photo/1",
        "https://www.twitter.com/alice/status/123/video/1",
        "https://mobile.twitter.com/alice/status/123",
        "https://x.com/i/web/status/123",
    ] {
        let url = url::Url::parse(input).unwrap();
        assert_eq!(super::x_post_id(&url), Some("123"), "{input}");
        assert!(super::is_external_embed(&url));
    }
    for input in [
        "ftp://x.com/alice/status/123",
        "https://x.com.evil.test/alice/status/123",
        "https://evil.test/x.com/alice/status/123",
        "https://alice@x.com/alice/status/123",
        "https://x.com:444/alice/status/123",
        "https://x.com/alice",
        "https://x.com/alice/status/",
        "https://x.com/alice/status/0",
        "https://x.com/alice/status/123abc",
        "https://x.com/alice/status/18446744073709551616",
        "https://x.com/alice/status/123/evil",
        "https://x.com/alice/status/123%22",
        "https://x.com/%22/status/123",
    ] {
        assert!(
            super::x_post_id(&url::Url::parse(input).unwrap()).is_none(),
            "{input}"
        );
    }
}

#[test]
fn x_post_widget_is_inert_until_expansion_and_uses_only_fixed_provider_origin() {
    use scraper::{Html, Selector};
    let html = super::x_post_html("123", "<script>caption</script>", true);
    let document = Html::parse_fragment(&html);
    let wrapper = document
        .select(&Selector::parse(".lazyload-wrapper").unwrap())
        .next()
        .unwrap();
    assert!(!wrapper.value().classes().any(|class| class == "-expanded"));
    assert_eq!(
        document.select(&Selector::parse("script").unwrap()).count(),
        0
    );
    assert!(
        document
            .select(&Selector::parse(".lazyload-caption").unwrap())
            .next()
            .unwrap()
            .text()
            .collect::<String>()
            .contains("<script>caption</script>")
    );
    let frame = document
        .select(&Selector::parse("iframe").unwrap())
        .next()
        .unwrap();
    assert!(frame.value().attr("src").is_none());
    assert!(frame.value().attr("srcdoc").is_none());
    assert!(frame.value().attr("hidden").is_some());
    assert_eq!(frame.value().attr("referrerpolicy"), Some("no-referrer"));
    assert_eq!(
        frame.value().attr("sandbox"),
        Some("allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox")
    );
    assert_eq!(
        frame.value().attr("data-src"),
        Some("https://platform.twitter.com/embed/Tweet.html?id=123&dnt=true&embedId=rostra-x-123")
    );
    assert_eq!(
        document
            .select(&Selector::parse("a").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("href"),
        Some("https://twitter.com/i/web/status/123")
    );
    assert!(!html.contains("IP address"));
    assert!(!html.contains("data-srcdoc"));
    assert_eq!(document.select(&Selector::parse("a").unwrap()).count(), 1);
    assert!(!html.contains("Open post on X"));
    assert!(frame.value().attr("data-x-post").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn production_external_media_cards_are_inert_escaped_and_context_scoped() {
    use rostra_core::id::RostraIdSecretKey;
    use scraper::{Html, Selector};

    let id = RostraIdSecretKey::generate().id();
    let client = rostra_client::Client::builder(id)
        .db(rostra_client_db::Database::new_in_memory(id).await.unwrap())
        .start_background_tasks(false)
        .start_request_handler(false)
        .build()
        .await
        .unwrap();
    let handle = client.handle();
    let client_ref = handle.client_ref().unwrap();
    let content = r#"
![X caption](https://x.com/alice/status/123)

![X caption](https://x.com/alice/status/123)

![YouTube caption](https://www.youtube.com/watch?v=a%22%3E%3Cscript%3E)

![Image <script> & caption](https://example.test/image.png?x=%22)

![](https://youtu.be/abc)

[ordinary YouTube](https://youtu.be/abc)

[ordinary X](https://x.com/alice/status/456)
"#;
    for allow in [true, false] {
        let renderer = make_base_renderer(
            jotup::html::tokio::Renderer::default()
                .rostra_profile_links(client_ref.clone())
                .rostra_media(client_ref.clone(), id)
                .external_media(allow),
        );
        let out = renderer.render_into_document(content).await.unwrap();
        let html = String::from_utf8(out.into_inner()).unwrap();
        let document = Html::parse_fragment(&html);
        let select = |s| Selector::parse(s).unwrap();
        assert_eq!(document.select(&select(".lazyload-wrapper")).count(), 6);
        assert_eq!(
            document.select(&select(".lazyload-message[href]")).count(),
            6
        );
        assert_eq!(document.select(&select(".lazyload-caption")).count(), 4);
        assert_eq!(
            document
                .select(&select("script, [onclick], [src], [srcdoc], [srcset]"))
                .count(),
            0
        );
        assert_eq!(
            document.select(&select("[data-src][hidden]")).count(),
            if allow { 6 } else { 0 }
        );
        assert_eq!(
            document.select(&select("[x-data]")).count(),
            if allow { 6 } else { 0 }
        );
        assert!(html.contains("ordinary YouTube"));
        assert!(html.contains("ordinary X"));
        if allow {
            let youtube = document
                .select(&select("iframe[title='YouTube video']"))
                .next()
                .unwrap();
            let url = url::Url::parse(youtube.value().attr("data-src").unwrap()).unwrap();
            assert_eq!(url.host_str(), Some("www.youtube.com"));
            assert_eq!(url.path(), "/embed/a%22%3E%3Cscript%3E");
        }
    }
}

#[test]
fn test_extract_rostra_media_link() {
    assert_eq!(
        UiState::extract_rostra_media_link(&format!("rostra-media:{TEST_EVENT_ID}")),
        Some(rostra_core::ShortEventId::from_str(TEST_EVENT_ID).unwrap())
    );
    assert_eq!(UiState::extract_rostra_media_link("not-a-media-link"), None);
}

#[test]
fn x_post_frame_keeps_a_transparent_canvas_without_clipping_dimensions() {
    let stylesheet = include_str!("../../../assets/style.css");
    let declarations = stylesheet
        .split(".m-xPostFrame {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(declarations.contains("color-scheme: light;"));
    assert!(!declarations.contains("color-scheme: normal;"));
    assert!(!declarations.contains("border-radius:"));
    assert!(declarations.contains("background: transparent;"));
    assert!(declarations.contains("width: 100%;"));
    assert!(declarations.contains("max-width: 550px;"));
    assert!(!declarations.contains("height:"));
    assert!(!declarations.contains("overflow:"));
}

#[test]
fn post_content_images_keep_intrinsic_width() {
    let stylesheet = include_str!("../../../assets/style.css");

    fn has_declaration(declarations: &str, property: &str, value: &str) -> bool {
        let declarations = declarations
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        declarations.split(';').any(|declaration| {
            let declaration = declaration
                .rsplit_once('{')
                .map_or(declaration, |(_, declaration)| declaration);
            declaration
                .split_once(':')
                .is_some_and(|(name, declared_value)| name == property && declared_value == value)
        })
    }

    fn has_image_element_selector(selector: &str) -> bool {
        selector
            .split(|character: char| {
                character.is_whitespace() || matches!(character, '>' | '+' | '~' | ',')
            })
            .any(|compound| {
                compound.strip_prefix("img").is_some_and(|suffix| {
                    suffix.chars().next().is_none_or(|character| {
                        !(character.is_ascii_alphanumeric() || character == '-' || character == '_')
                    })
                })
            })
    }

    fn has_nested_image_element_selector(declarations: &str) -> bool {
        declarations.split_once('{').is_some_and(|(prefix, _)| {
            prefix.rsplit(';').next().is_some_and(|selector| {
                selector
                    .trim()
                    .strip_prefix('&')
                    .is_some_and(has_image_element_selector)
            })
        })
    }

    assert!(
        stylesheet
            .split('}')
            .filter_map(|rule| rule.split_once('{'))
            .any(|(selector, declarations)| {
                selector.trim() == "img" && has_declaration(declarations, "max-width", "100%")
            }),
        "images should shrink to their container"
    );

    assert!(
        !stylesheet
            .split('}')
            .filter_map(|rule| rule.split_once('{'))
            .any(|(selector, declarations)| {
                selector.contains(".m-postView__content")
                    && (has_image_element_selector(selector)
                        || has_nested_image_element_selector(declarations))
                    && has_declaration(declarations, "width", "100%")
            }),
        "post content images should not expand beyond their intrinsic width"
    );

    for selector in [
        ".m-postView__content .m-rostraMedia img",
        ".m-postView__content .lazyload-wrapper img",
    ] {
        assert!(
            stylesheet
                .split('}')
                .filter_map(|rule| rule.split_once('{'))
                .any(|(candidate, declarations)| {
                    candidate
                        .split(',')
                        .any(|candidate| candidate.trim() == selector)
                        && has_declaration(declarations, "display", "block")
                        && has_declaration(declarations, "margin-inline", "auto")
                }),
            "post images matching {selector} should be centered"
        );
    }
}

#[test]
fn rendered_post_headings_use_a_scoped_geometric_scale() {
    let stylesheet = include_str!("../../../assets/style.css");
    let post_content_scopes = [".m-postView__content", ".o-shoutbox__postContent"];
    let sizes: [f64; 5] = [1.50, 1.39, 1.28, 1.19, 1.10];

    for (level, size) in (1..=5).zip(sizes) {
        for scope in post_content_scopes {
            let selector = format!("{scope} h{level}");
            assert!(
                stylesheet
                    .split('}')
                    .filter_map(|rule| rule.split_once('{'))
                    .any(|(selectors, declarations)| {
                        selectors
                            .split(',')
                            .any(|candidate| candidate.trim() == selector)
                            && declarations.contains(&format!("font-size: {size:.2}rem"))
                    }),
                "missing scoped heading rule for {selector}"
            );
        }
    }

    let ratios = sizes.windows(2).map(|sizes| sizes[1] / sizes[0]);
    for ratio in ratios {
        assert!(
            (ratio - 0.925).abs() < 0.01,
            "heading sizes should approximate a geometric progression"
        );
    }
}

#[test]
fn post_avatar_aligns_to_the_top_of_the_header_row() {
    let stylesheet = include_str!("../../../assets/style.css");

    assert!(
        stylesheet
            .split('}')
            .filter_map(|rule| rule.split_once('{'))
            .any(|(selector, declarations)| {
                selector.trim() == ".m-postView__userImage"
                    && declarations.contains("align-self: flex-start")
            }),
        "post avatars should remain top-aligned when the adjacent content grows"
    );
}

/// Helper to render djot content with code block filter only
async fn render_with_prism(content: &str) -> String {
    let renderer = jotup::html::tokio::Renderer::default().prism_code_blocks();

    let out = renderer
        .render_into_document(content)
        .await
        .expect("Rendering failed");

    String::from_utf8(out.into_inner()).expect("valid utf8")
}

// Note: Tests for rostra-media rendering and external image lazy-loading
// require a database client and are tested via integration tests.

#[tokio::test]
async fn code_block_gets_prism_classes() {
    let content = "```rust\nfn main() {}\n```";

    let html = render_with_prism(content).await;

    assert!(
        html.contains("language-rust"),
        "Missing language-rust class"
    );
}

#[tokio::test]
async fn code_block_unknown_language() {
    let content = "```\nplain code\n```";

    let html = render_with_prism(content).await;

    assert!(html.contains("<code"), "Missing code element");
}

#[tokio::test]
async fn inline_code_not_affected_by_prism() {
    let content = "Some `inline code` here";

    let html = render_with_prism(content).await;

    assert!(
        !html.contains("language-"),
        "Inline code should not have language class"
    );
    assert!(
        html.contains("<code>inline code</code>"),
        "Missing inline code"
    );
}

/// Helper to render djot content and see raw djot events
fn render_events(content: &str) -> Vec<jotup::Event<'_>> {
    jotup::Parser::new(content).collect()
}

#[test]
fn djot_image_with_apostrophe_events() {
    // Test that djot parses apostrophes in image alt text as separate events.
    // This is important because our RostraMedia filter must handle smart
    // punctuation events (like RightSingleQuote) inside alt text, not pass them
    // through.
    let content = r#"![I'ts](https://www.youtube.com/watch?v=Z0GFRcFm-aY)"#;
    let events = render_events(content);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightSingleQuote)),
        "Expected RightSingleQuote event for the apostrophe"
    );

    let str_contents: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            jotup::Event::Str(s) => Some(s.as_ref()),
            _ => None,
        })
        .collect();
    assert!(
        str_contents.contains(&"I"),
        "Expected 'I' before apostrophe"
    );
    assert!(
        str_contents.contains(&"ts"),
        "Expected 'ts' after apostrophe"
    );
}

#[test]
fn djot_image_with_multiple_smart_punctuation() {
    let content = r#"![It's "great"...](https://example.com/img.png)"#;
    let events = render_events(content);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightSingleQuote))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::LeftDoubleQuote))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, jotup::Event::RightDoubleQuote))
    );
    assert!(events.iter().any(|e| matches!(e, jotup::Event::Ellipsis)));
}

#[test]
fn djot_image_with_softbreak_and_symbol() {
    let content = "![line1\nline2](https://example.com/img.png)";
    let events = render_events(content);

    assert!(
        events.iter().any(|e| matches!(e, jotup::Event::Softbreak)),
        "Expected Softbreak event for newline in alt text"
    );

    let content_sym = "![a :smile: emoji](https://example.com/img.png)";
    let events_sym = render_events(content_sym);

    assert!(
        events_sym
            .iter()
            .any(|e| matches!(e, jotup::Event::Symbol(_))),
        "Expected Symbol event for :smile: in alt text"
    );
}

/// Helper to render djot content with full sanitization (like production).
/// Uses the same sanitization chain as production code via
/// `make_base_renderer`.
pub(super) async fn render_sanitized(content: &str) -> String {
    let out = make_base_renderer(jotup::html::tokio::Renderer::default())
        .render_into_document(content)
        .await
        .expect("Rendering failed");

    String::from_utf8(out.into_inner()).expect("valid utf8")
}
