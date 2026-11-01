use super::*;
use crate::native::{MAX_BLOCK_OPENERS, MAX_IMAGES, MAX_INPUT_BYTES};

#[test]
fn golden_documents() {
    for (name, source, expected) in [
        (
            "common",
            include_str!("fixtures/common.djot"),
            include_str!("fixtures/common.json"),
        ),
        (
            "links-media",
            include_str!("fixtures/links-media.djot"),
            include_str!("fixtures/links-media.json"),
        ),
        (
            "deferred",
            include_str!("fixtures/deferred.djot"),
            include_str!("fixtures/deferred.json"),
        ),
        (
            "references",
            include_str!("fixtures/references.djot"),
            include_str!("fixtures/references.json"),
        ),
    ] {
        let document = render_semantic(source).unwrap();
        assert_eq!(
            serde_json::to_value(document).unwrap(),
            serde_json::from_str::<serde_json::Value>(expected).unwrap(),
            "{name}",
        );
    }
}

#[test]
fn guards_fail_atomically_after_valid_prefixes() {
    assert_eq!(
        render_semantic(&"a".repeat(MAX_INPUT_BYTES + 1)),
        Err(NativeRenderError::InputLimit),
    );
    assert_eq!(
        render_semantic(&format!("valid\n\n{}", "> ".repeat(MAX_BLOCK_OPENERS + 1))),
        Err(NativeRenderError::ComplexityLimit),
    );
    assert_eq!(
        render_semantic(&format!(
            "valid\n\n{}",
            "![alt](https://example.org/i) ".repeat(MAX_IMAGES + 1)
        )),
        Err(NativeRenderError::ImageLimit),
    );
    let rows = format!("valid\n\n{}", "| a |\n".repeat(MAX_TABLE_ROWS + 1));
    assert_eq!(render_semantic(&rows), Err(NativeRenderError::TableLimit));
    let cells = format!("valid\n\n|{}|\n", "a|".repeat(MAX_TABLE_CELLS + 1));
    assert_eq!(render_semantic(&cells), Err(NativeRenderError::TableLimit));
}

#[test]
fn empty_and_unicode_text_are_owned_without_offsets() {
    assert_eq!(
        render_semantic("").unwrap(),
        SemanticDocument {
            version: 1,
            nodes: vec![],
            warnings: vec![],
        }
    );
    let document = render_semantic("é🙂 *中文*").unwrap();
    assert!(document.warnings.is_empty());
    assert_eq!(
        document.nodes[0].children[0].kind,
        SemanticKind::Text {
            text: "é🙂 ".into()
        }
    );
}

#[test]
fn depth_nodes_text_and_repeated_destinations_are_bounded() {
    use rostra_core::id::ToShort as _;

    use crate::native::{MAX_DEPTH, MAX_SPANS};

    assert_eq!(
        render_semantic(&format!("{}text", "> ".repeat(MAX_DEPTH))),
        Err(NativeRenderError::DepthLimit),
    );
    // Three nodes per marked word: mark, text, trailing plain text.
    assert_eq!(
        render_semantic(&"*x* ".repeat(MAX_NODES / 3 + 1)),
        Err(NativeRenderError::SpanLimit),
    );
    assert_eq!(
        render_semantic(&format!("start{}end", "\\\n".repeat(MAX_NODES))),
        Err(NativeRenderError::NodeLimit),
    );
    // Transparent spans still require independent owned text leaves.
    assert_eq!(
        render_semantic(&"[x]{.ignored}".repeat(MAX_SPANS + 1)),
        Err(NativeRenderError::SpanLimit),
    );
    let author = rostra_core::id::RostraId::from_bytes([7; 32]).to_short();
    let event = rostra_core::ShortEventId::from_bytes([9; 16]);
    assert_eq!(
        render_semantic(&format!("[x](/post/{author}/{event}) ").repeat(600)),
        Err(NativeRenderError::OutputLimit),
    );
}

#[test]
fn table_and_image_budgets_accept_boundary_and_count_across_blocks() {
    assert!(render_semantic(&"| a |\n".repeat(MAX_TABLE_ROWS)).is_ok());
    assert!(render_semantic(&format!("|{}\n", "a|".repeat(MAX_TABLE_CELLS))).is_ok());
    assert_eq!(
        render_semantic(&"|a|\n\nseparate\n\n".repeat(MAX_TABLE_ROWS + 1)),
        Err(NativeRenderError::TableLimit),
    );
    assert!(render_semantic(&"![](https://example.org/i)".repeat(MAX_IMAGES)).is_ok());
}

#[test]
fn literal_and_alternative_contexts_never_produce_descendant_actions() {
    for (source, expected_images) in [
        (
            "`[link](https://example.org/) ![image](https://example.org/i)`",
            0,
        ),
        (
            "```=html\n<script src='https://example.org/'></script>\n```",
            0,
        ),
        (
            "![alternative [link](https://example.org/)](https://example.org/i)",
            1,
        ),
        (
            "![outer ![inner](https://example.org/inner)](https://example.org/outer)",
            1,
        ),
        ("$`[link](https://example.org/)`", 0),
        (
            "[^n]: [link](https://example.org/) ![image](https://example.org/i)",
            0,
        ),
    ] {
        let document = render_semantic(source).unwrap();
        let mut nodes: Vec<_> = document.nodes.iter().collect();
        let mut images = 0;
        while let Some(node) = nodes.pop() {
            assert!(!matches!(
                node.kind,
                SemanticKind::Link {
                    destination: Some(_),
                    ..
                }
            ));
            images += usize::from(matches!(node.kind, SemanticKind::Image { .. }));
            nodes.extend(&node.children);
        }
        assert_eq!(images, expected_images, "{source}");
    }
}
