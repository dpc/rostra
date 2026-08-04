use rostra_core::id::RostraId;

use super::requested_author_matches_event;

#[test]
fn retained_event_cannot_be_rendered_as_another_author() {
    let requested_author = RostraId::from_bytes([42; 32]);
    let actual_author = RostraId::from_bytes([43; 32]);

    assert!(requested_author_matches_event(requested_author, None));
    assert!(requested_author_matches_event(
        requested_author,
        Some(requested_author)
    ));
    assert!(!requested_author_matches_event(
        requested_author,
        Some(actual_author)
    ));
}
