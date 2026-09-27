use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use scraper::{Html, Selector};

use super::{MediaInfo, render_media_list};

fn render(target: &str) -> String {
    render_media_list(
        RostraId::from_bytes([42; 32]),
        target,
        &[MediaInfo {
            event_id: ShortEventId::from_bytes([43; 16]),
            mime: "application/octet-stream".to_owned(),
            size: 3,
            is_image: false,
            is_video: false,
        }],
    )
    .0
    .into_string()
}

#[test]
fn media_target_remains_data_only() {
    let targets = [
        "#composer",
        "apostrophe'payload",
        "double\"quote",
        r"back\slash",
        "ampersand&payload",
        "<angle>",
        "line\nbreak",
        "');globalThis.executionSentinel++;function insertMediaSyntax(){}//",
    ];
    let media_list_selector = Selector::parse("#media-list").unwrap();
    let item_selector = Selector::parse(".o-mediaList__item").unwrap();
    let event_id = ShortEventId::from_bytes([43; 16]);
    let expected_handler = format!(
        "insertMediaSyntax('{event_id}'); document.getElementById('media-list').classList.remove('-active')"
    );

    for target in targets {
        let document = Html::parse_fragment(&render(target));
        let media_list = document
            .select(&media_list_selector)
            .next()
            .expect("media list");
        assert_eq!(media_list.value().attr("data-target"), Some(target));

        let item = document.select(&item_selector).next().expect("media item");
        assert_eq!(item.value().name(), "button");
        assert_eq!(item.value().attr("type"), Some("button"));
        assert_eq!(
            item.value().attr("aria-label"),
            Some("Attach application/octet-stream (3 B)")
        );
        let handler = item.value().attr("onclick").expect("media item handler");
        assert_eq!(handler, expected_handler);
        assert!(
            !handler.contains(target),
            "target must not become handler program text: {target:?}"
        );
    }
}
