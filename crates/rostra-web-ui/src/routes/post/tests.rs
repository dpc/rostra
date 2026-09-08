use std::collections::BTreeSet;

use rostra_client_db::social::SocialPostRecord;
use rostra_core::event::SocialPost;
use rostra_core::id::RostraId;
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

use super::{find_own_reaction, is_own_reaction, requested_author_matches_event};

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

#[test]
fn own_reaction_to_an_older_post_version_remains_deletable_by_precise_id() {
    let current_user = RostraId::from_bytes([42; 32]);
    let post_author = RostraId::from_bytes([43; 32]);
    let older_post_version = ExternalEventId::new(post_author, ShortEventId::from_bytes([44; 16]));
    let reaction_event_id = ShortEventId::from_bytes([45; 16]);
    let reaction = SocialPostRecord {
        ts: Timestamp::ZERO,
        event_id: reaction_event_id,
        author: current_user,
        reply_to: Some(older_post_version),
        content: SocialPost::new("❤️".to_owned(), Some(older_post_version), BTreeSet::new()),
        reply_count: 0,
    };

    assert!(is_own_reaction(&reaction, current_user, reaction_event_id));
    assert!(
        find_own_reaction(
            std::slice::from_ref(&reaction),
            current_user,
            reaction_event_id
        )
        .is_some(),
        "selection from the displayed replacement chain must accept the older target"
    );
    assert!(!is_own_reaction(
        &reaction,
        RostraId::from_bytes([46; 32]),
        reaction_event_id
    ));
    assert!(!is_own_reaction(
        &reaction,
        current_user,
        ShortEventId::from_bytes([47; 16])
    ));

    let reply = SocialPostRecord {
        content: SocialPost::new_text(
            "not a reaction".to_owned(),
            Some(older_post_version),
            BTreeSet::new(),
        ),
        ..reaction
    };
    assert!(!is_own_reaction(&reply, current_user, reaction_event_id));
}
