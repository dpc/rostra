use super::*;

#[test]
fn exact_full_short_mixed_components_round_trip_without_shortening() {
    let author = RostraId::from_bytes([7; 32]);
    let event = EventId::from_bytes([9; 32]);
    for author in [
        PostAuthorReference::Full(author),
        PostAuthorReference::Short(author.into()),
    ] {
        for event in [
            PostEventReference::Full(event),
            PostEventReference::Short(event.into()),
        ] {
            let reference = PostReference { author, event };
            assert_eq!(reference.to_string().parse(), Ok(reference));
        }
    }
}

#[test]
fn no_aliases_normalization_origin_or_suffix_semantics() {
    let author = RostraId::from_bytes([7; 32]);
    let event = EventId::from_bytes([9; 32]);
    let path = format!("/post/{author}/{event}");
    for invalid in [
        format!("https://example.org{path}"),
        format!("http://example.org{path}"),
        format!("//example.org{path}"),
        path.trim_start_matches('/').to_owned(),
        format!("/{path}"),
        format!("{path}/"),
        format!("{path}/edit"),
        format!("{path}/delete"),
        format!("{path}/fetch"),
        format!("{path}?raw=true"),
        format!("{path}#fragment"),
        format!(" {path}"),
        format!("{path}\n"),
        path.replace("/post/", "/post//"),
        path.replace("/post/", "/post/../post/"),
        path.replace("/post/", "/%70ost/"),
        path.replace("/post/", "/post%2f"),
        path.replace('/', "\\"),
        format!(
            "/post/{}/{event}",
            author.to_string().trim_start_matches("rs")
        ),
        format!("/post/{author}/{}", event.to_string().to_lowercase()),
        format!("/post/{author}/{event}%00"),
        format!("/post/{author}/{}", &event.to_string()[..25]),
        format!("/post/{}x/{event}", ShortRostraId::from(author)),
    ] {
        assert!(invalid.parse::<PostReference>().is_err(), "{invalid:?}");
    }
}
