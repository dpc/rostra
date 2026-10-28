use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::media::MediaEventReference;

use super::*;

#[test]
fn post_links_preserve_exact_destinations_and_existing_inert_boundaries() {
    use rostra_core::post::{PostAuthorReference, PostEventReference, PostReference};

    let author = RostraId::from_bytes([7; 32]);
    let event = rostra_core::EventId::from_bytes([9; 32]);
    for author in [
        PostAuthorReference::Full(author),
        PostAuthorReference::Short(author.to_short()),
    ] {
        for event in [
            PostEventReference::Full(event),
            PostEventReference::Short(event.to_short()),
        ] {
            let reference = PostReference { author, event };
            let path = reference.to_string();
            let doc = render(&format!(
                "[not the author]({path}) and [ref][p]\n\n[p]: {path}"
            ))
            .unwrap();
            assert_eq!(text(&doc), "not the author and ref");
            assert_eq!(doc.spans.iter().filter(|s| s.link.is_some()).count(), 2);
            assert!(
                doc.spans
                    .iter()
                    .filter_map(|s| s.link.as_ref())
                    .all(|link| *link == NativeLink::Post { reference })
            );
            let json = serde_json::to_value(&doc).unwrap();
            assert_eq!(json["spans"][0]["link"]["kind"], "post");
            assert_eq!(
                json["spans"][0]["link"]["reference"]["event"]["id"],
                event.to_string()
            );
            for source in [
                format!("`[code]({path})`"),
                format!("```html\n<a href=\"{path}\">raw</a>\n```"),
                format!("```=html\n<a href=\"{path}\">raw</a>\n```"),
                format!("[undefined][{path}]"),
                format!("[query]({path}?raw=true)"),
                format!("[fragment]({path}#x)"),
                format!("[suffix]({path}/edit)"),
                format!("[encoded]({})", path.replace("/post/", "/%70ost/")),
            ] {
                let doc = render(&source).unwrap();
                assert!(doc.spans.iter().all(|s| s.link.is_none()), "{source}");
            }
            let external = render(&format!("[label](https://example.org{path})")).unwrap();
            assert!(matches!(
                external.spans[0].link,
                Some(NativeLink::External { .. })
            ));
            let image = render_with_media(&format!("![alt]({path})")).unwrap();
            assert_eq!(image.images[0].destination, NativeImageDestination::Invalid);
        }
    }
}

#[test]
fn repeated_post_destinations_share_the_atomic_output_budget() {
    let author = RostraId::from_bytes([7; 32]).to_short();
    let event = rostra_core::ShortEventId::from_bytes([9; 16]);
    let source = format!("[x](/post/{author}/{event}) ").repeat(600);
    assert!(source.len() < MAX_INPUT_BYTES);
    assert_eq!(render(&source), Err(NativeRenderError::OutputLimit));
}

#[test]
fn opt_in_images_preserve_order_alt_empty_anchors_and_destination_classes() {
    let id = rostra_core::EventId::from_bytes([7; 32]);
    let short = id.to_short();
    let source = format!(
        "before ![one *bold*](rostra-media:{short})![](rostra-media:{id}) after ![remote](https://example.com/a.png) ![bad](javascript:alert) ![unknown][missing]"
    );
    let doc = render_with_media(&source).unwrap();
    assert_eq!(doc.images.len(), 5);
    assert_eq!(doc.images[0].alt, "one bold");
    assert_eq!(doc.images[1].alt, "");
    assert_eq!(doc.images[0].span_index, doc.images[1].span_index);
    assert_eq!(
        doc.images[0].destination,
        NativeImageDestination::Rostra {
            reference: MediaEventReference::Short(short)
        }
    );
    assert_eq!(
        doc.images[1].destination,
        NativeImageDestination::Rostra {
            reference: MediaEventReference::Full(id)
        }
    );
    assert!(matches!(
        doc.images[2].destination,
        NativeImageDestination::External { .. }
    ));
    assert_eq!(doc.images[3].destination, NativeImageDestination::Invalid);
    assert_eq!(doc.images[4].destination, NativeImageDestination::Invalid);
    assert!(doc.spans.iter().all(|s| s.link.is_none()));
    assert!(!text(&doc).contains("one"));
    assert_eq!(
        text(&render(&source).unwrap()).matches("[image]").count(),
        5
    );
    assert!(render(&source).unwrap().images.is_empty());
    let mut combined = String::new();
    for index in 0..=doc.spans.len() {
        for image in doc.images.iter().filter(|image| image.span_index == index) {
            combined.push_str(&format!("[{}]", image.alt));
        }
        if let Some(span) = doc.spans.get(index) {
            combined.push_str(&span.text);
        }
    }
    assert_eq!(
        combined,
        "before [one bold][] after [remote] [bad] [unknown]"
    );
    let json = serde_json::to_value(&doc).unwrap();
    assert_eq!(
        json["images"][0]["destination"]["reference"]["kind"],
        "short"
    );
    assert_eq!(
        json["images"][1]["destination"]["reference"]["kind"],
        "full"
    );
}

#[test]
fn media_mode_keeps_guards_inert_markup_and_atomic_limits() {
    assert_eq!(
        render_with_media(&"x".repeat(MAX_INPUT_BYTES + 1)),
        Err(NativeRenderError::InputLimit)
    );
    assert_eq!(
        render_with_media(&"- x\n".repeat(MAX_BLOCK_OPENERS + 1)),
        Err(NativeRenderError::ComplexityLimit)
    );
    assert_eq!(
        render_with_media(&"![](https://example.com/x)".repeat(MAX_IMAGES + 1)),
        Err(NativeRenderError::ImageLimit)
    );
    assert_eq!(
        render_with_media(&"![](https://example.com/x)".repeat(MAX_IMAGES))
            .unwrap()
            .images
            .len(),
        MAX_IMAGES
    );
    for destination in [
        "data:image/png,x",
        "file:///tmp/a",
        "https://user:pass@example.com/a",
        "rostra-media:unknown",
    ] {
        let doc = render_with_media(&format!("![alt]({destination})")).unwrap();
        assert_eq!(doc.images[0].destination, NativeImageDestination::Invalid);
    }
    for source in [
        "`![alt](https://example.com/a)`",
        "`<img src='https://example.com/a'>`{=html}",
    ] {
        assert!(render_with_media(source).unwrap().images.is_empty());
    }
}

fn text(document: &NativeDocument) -> String {
    document
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect()
}

#[test]
fn plain_unicode_and_nested_formatting() {
    let source = "Hello 世界 👩🏽‍💻";
    assert_eq!(text(&render(source).unwrap()), source);
    let doc = render("plain *strong _both_* and `code`").unwrap();
    assert_eq!(text(&doc), "plain strong both and code");
    assert!(
        doc.spans
            .iter()
            .any(|s| s.text == "both" && s.strong && s.emphasis)
    );
    assert!(
        doc.spans
            .iter()
            .any(|s| s.text == "code" && s.code && s.link.is_none())
    );
    assert!(doc.warnings.is_empty());
}

#[test]
fn blocks_remain_legible_without_a_trailing_separator() {
    let doc = render("# Heading\n\n> quote\n\n- first\n- second\n\nlast").unwrap();
    assert_eq!(text(&doc), "# Heading\n> quote\n• first\n• second\nlast");
}

#[test]
fn identity_references_are_never_promoted_or_inferred_from_labels() {
    let id = RostraId::from_bytes([42; 32]);
    let other = RostraId::from_bytes([43; 32]);
    let doc = render(&format!(
        "[{other}](rostra:{id}) [short](rostra:{}) [unknown](rostra:nope) @someone",
        id.to_short()
    ))
    .unwrap();
    let links: Vec<_> = doc.spans.iter().filter_map(|s| s.link.clone()).collect();
    assert_eq!(
        links,
        [
            NativeLink::Profile {
                reference: RostraIdLink::Full(id)
            },
            NativeLink::Profile {
                reference: RostraIdLink::Short(id.to_short())
            },
        ]
    );
    assert!(doc.warnings.contains(&NativeWarning::InvalidLink));
    assert!(text(&doc).contains("@someone"));
}

#[test]
fn external_destinations_are_explicit_canonical_and_credential_free() {
    let doc = render("[label](HTTPS://ExAmPle.com/世界?q=x#fragment)").unwrap();
    assert_eq!(
        doc.spans[0].link,
        Some(NativeLink::External {
            url: "https://example.com/%E4%B8%96%E7%95%8C?q=x#fragment".into(),
        })
    );
    for destination in [
        "javascript:alert(1)",
        "data:text/html,bad",
        "file:///etc/passwd",
        "intent://host",
        "//example.com",
        "/profile/id",
        "https:example.com",
        "https://user:password@example.com",
        "https://user@example.com",
        "rostra-media:bad",
        "mailto:somebody@example.com",
        "https://",
    ] {
        let doc = render(&format!("[label]({destination})")).unwrap();
        assert!(doc.spans.iter().all(|s| s.link.is_none()), "{destination}");
    }
}

#[test]
fn references_autolinks_and_unsafe_encoded_destinations() {
    let doc =
        render("[named][target] <https://example.com/>\n\n[target]: https://example.org/").unwrap();
    assert_eq!(text(&doc), "named https://example.com/");
    assert_eq!(doc.spans.iter().filter(|s| s.link.is_some()).count(), 2);
    for destination in [
        "https://example.com\\@evil.test",
        "https://example.com/\u{7f}bad",
        "https://example.com/\tbad",
        "javascript%3Aalert(1)",
        "&#106;avascript:alert(1)",
    ] {
        let doc = render(&format!("[label]({destination})")).unwrap();
        assert!(
            doc.spans.iter().all(|s| s.link.is_none()),
            "{destination:?}"
        );
    }
    let doc = render(&format!("[long](https://example.com/{})", "a".repeat(2048))).unwrap();
    assert!(doc.spans.iter().all(|s| s.link.is_none()));
    assert!(doc.warnings.contains(&NativeWarning::InvalidLink));
}

#[test]
fn serialized_segments_preserve_unicode_and_canonical_reference_classes() {
    let id = RostraId::from_bytes([42; 32]);
    let doc = render(&format!("[世界](rostra:{})", id.to_short())).unwrap();
    let json = serde_json::to_value(doc).unwrap();
    assert_eq!(json["spans"][0]["text"], "世界");
    assert_eq!(json["spans"][0]["link"]["kind"], "profile");
    assert_eq!(
        json["spans"][0]["link"]["reference"],
        id.to_short().to_string()
    );
    assert_eq!(json["warnings"], serde_json::json!([]));
    assert_eq!(
        serde_json::to_value(NativeRenderError::ComplexityLimit).unwrap(),
        "complexity_limit"
    );
}

#[test]
fn unresolved_reference_labels_do_not_become_navigation_destinations() {
    let id = RostraId::from_bytes([42; 32]);
    for source in [
        "[label][https://example.com]".to_owned(),
        format!("[label][rostra:{id}]"),
        format!("[rostra:{id}][]"),
    ] {
        let doc = render(&source).unwrap();
        assert!(doc.spans.iter().all(|span| span.link.is_none()));
        assert!(doc.warnings.contains(&NativeWarning::InvalidLink));
    }
}

#[test]
fn raw_markup_code_attributes_and_images_are_inert() {
    let id = RostraId::from_bytes([42; 32]);
    let source = format!(
        "`[mention](rostra:{id})`\n\n```=html\n<script src=\"https://evil.test\">bad</script>\n```\n\n![alt](https://evil.test/image)\n\n[text]{{onclick=\"bad\"}}"
    );
    let doc = render(&source).unwrap();
    assert!(doc.spans.iter().all(|s| s.link.is_none()));
    assert!(
        doc.spans
            .iter()
            .any(|s| s.code && s.text.contains("<script"))
    );
    assert!(text(&doc).contains("[image] alt"));
    for warning in [
        NativeWarning::RawMarkup,
        NativeWarning::MediaOmitted,
        NativeWarning::AttributesIgnored,
    ] {
        assert!(doc.warnings.contains(&warning), "{doc:?}");
    }
    let doc = render("[inside ![alt](https://image.test)](https://outer.test)").unwrap();
    assert!(
        doc.spans
            .iter()
            .filter(|s| s.text.contains("alt"))
            .all(|s| s.link.is_none())
    );
}

#[test]
fn unsupported_constructs_warn_instead_of_disappearing() {
    let doc = render("a ~sub~ ^super^ {=marked=}\n\n|a|b|\n|c|d|").unwrap();
    assert!(doc.warnings.contains(&NativeWarning::UnsupportedMarkup));
    for expected in ["sub", "super", "marked", "a", "b", "c", "d"] {
        assert!(text(&doc).contains(expected));
    }
    for source in ["## Subheading", "10. numbered\n11. next"] {
        let doc = render(source).unwrap();
        assert!(doc.warnings.contains(&NativeWarning::UnsupportedMarkup));
        assert!(!text(&doc).is_empty());
    }
}

#[test]
fn input_and_preparser_complexity_boundaries() {
    assert!(render(&"a".repeat(MAX_INPUT_BYTES)).is_ok());
    assert_eq!(
        render(&"a".repeat(MAX_INPUT_BYTES + 1)),
        Err(NativeRenderError::InputLimit)
    );
    assert!(render(&"- item\n".repeat(MAX_BLOCK_OPENERS)).is_ok());
    assert_eq!(
        render(&"- item\n".repeat(MAX_BLOCK_OPENERS + 1)),
        Err(NativeRenderError::ComplexityLimit)
    );
    assert_eq!(
        render(&"#".repeat(MAX_INPUT_BYTES)),
        Err(NativeRenderError::ComplexityLimit)
    );
    // Plain inline formatting and URL punctuation do not consume the block budget.
    let ordinary = "*bold* _italic_ [link](https://example.com/a?q=x&v=y) ".repeat(100);
    assert!(render(&ordinary).is_ok());
    // Literal code may conservatively overcount block-looking text.
    assert_eq!(
        render(&format!("```\n{}```", "> quote\n".repeat(65))),
        Err(NativeRenderError::ComplexityLimit)
    );
}

#[test]
fn recursive_block_paths_are_rejected_before_parser_construction() {
    for source in [
        format!("{}text", "> ".repeat(65)),
        format!("{}text", "- ".repeat(65)),
        format!("{}text", "1. ".repeat(65)),
        format!("{}text", "(iv) ".repeat(65)),
        format!("{}text", ": ".repeat(65)),
        format!("{}text", "[^a]:".repeat(65)),
        format!("{}text", "- [x] ".repeat(65)),
        format!("{}\ntext\n", "::: class\n".repeat(65)),
        format!("{}世界", "\t>\t".repeat(65)),
        "> quote\r\n".repeat(65),
        format!("{}text", "> - [x] [^a]:1. : ".repeat(20)),
    ] {
        assert_eq!(
            crate::parse_guard::check(&source),
            Err(NativeRenderError::ComplexityLimit),
            "{source}"
        );
    }
}

#[test]
fn parsed_depth_and_owned_output_are_independently_bounded() {
    assert_eq!(
        render(&format!("{}text", "> ".repeat(64))),
        Err(NativeRenderError::DepthLimit)
    );
    let source = format!(
        "[{}](https://example.com/{})",
        "*x* _y_ ".repeat(100),
        "a".repeat(1900)
    );
    assert_eq!(render(&source), Err(NativeRenderError::OutputLimit));
    let source = "*a* b ".repeat(2100);
    assert_eq!(render(&source), Err(NativeRenderError::SpanLimit));
}
