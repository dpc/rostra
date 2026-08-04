use super::extract_social_excerpt;

#[test]
fn h1_and_h2_are_social_titles_but_h3_is_not() {
    assert_eq!(
        extract_social_excerpt("## Subtitle\n\nParagraph.")
            .first_heading
            .as_deref(),
        Some("Subtitle")
    );
    assert_eq!(
        extract_social_excerpt("### Detail\n\nParagraph.").first_heading,
        None
    );
}

#[test]
fn social_paragraphs_are_ordered_and_normalized() {
    let excerpt = extract_social_excerpt("One\u{2003} two\tthree\nfour.\n\nFive.");

    assert_eq!(excerpt.paragraphs, vec!["One two three four.", "Five."]);
}

#[test]
fn code_and_media_only_content_have_no_social_excerpt() {
    for content in [
        "``` rust\nfn main() {}\n```",
        "![](rostra-media:AAAAAAAAAAAAAAAAAAAAAAAAAA)",
    ] {
        let excerpt = extract_social_excerpt(content);
        assert_eq!(excerpt.first_heading, None);
        assert!(excerpt.paragraphs.is_empty());
    }
}
