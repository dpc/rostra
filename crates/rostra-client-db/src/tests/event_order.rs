use rostra_core::event::{
    Event, EventAuxKey, EventContentRaw, EventExt as _, EventKind, VerifiedEvent,
    VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_util_error::BoxedErrorResult;

use super::temp_db;
use crate::Database;

fn build_content_event_at<C>(
    id_secret: RostraIdSecretKey,
    content: &C,
    timestamp: i64,
) -> VerifiedEventContent
where
    C: content_kind::EventContentKind,
{
    build_content_event_with_parent_at(id_secret, content, None, timestamp)
}

fn build_content_event_with_parent_at<C>(
    id_secret: RostraIdSecretKey,
    content: &C,
    parent_prev: Option<rostra_core::ShortEventId>,
    timestamp: i64,
) -> VerifiedEventContent
where
    C: content_kind::EventContentKind,
{
    let author = id_secret.id();
    let (event, content_raw) = Event::builder(content)
        .author(author)
        .maybe_parent_prev(parent_prev)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .build()
        .expect("valid event content");
    let signed = event.signed_by(id_secret);
    let verified = VerifiedEvent::verify_signed(author, signed).expect("valid signed event");
    VerifiedEventContent::assume_verified(verified, content_raw)
}

fn build_social_post_singleton_event_at(
    id_secret: RostraIdSecretKey,
    aux_key: EventAuxKey,
    content: EventContentRaw,
    timestamp: i64,
) -> VerifiedEventContent {
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .singleton_aux_key(aux_key)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .content(&content)
        .build();
    let signed = event.signed_by(id_secret);
    let verified = VerifiedEvent::verify_signed(author, signed).expect("valid signed event");
    VerifiedEventContent::assume_verified(verified, content)
}

fn winning_event_index(events: &[VerifiedEventContent; 2]) -> usize {
    events
        .iter()
        .enumerate()
        .max_by_key(|(_, event)| event.event_id().to_short())
        .expect("two events")
        .0
}

/// Equal-second follow state and selector conflicts converge in both orders.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_equal_timestamp_follow_conflicts_converge() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind::Follow;
    use rostra_core::event::{PersonaId, PersonaSelector, PersonaTag, PersonasTagsSelector};

    use crate::{events_singletons_new, ids_followees, ids_followers, ids_unfollowed};

    let author_secret = RostraIdSecretKey::generate();
    let author = author_secret.id();
    let followee = RostraIdSecretKey::generate().id();
    let timestamp = 10_000;
    let only_personal = Some(PersonaSelector::Only {
        ids: vec![PersonaId(0)],
    });
    let except_professional = Some(PersonaSelector::Except {
        ids: vec![PersonaId(1)],
    });
    let singleton_key = (
        author,
        EventKind::FOLLOW,
        EventAuxKey::from_bytes(followee.to_short().to_bytes()),
    );

    for (left_selector, right_selector) in [
        (only_personal.clone(), None),
        (only_personal.clone(), except_professional.clone()),
        (None, None),
    ] {
        let events = [
            build_content_event_with_parent_at(
                author_secret,
                &Follow {
                    followee,
                    persona: None,
                    selector: left_selector.clone(),
                    persona_tags_selector: None,
                },
                Some(rostra_core::ShortEventId::from_bytes([3; 16])),
                timestamp,
            ),
            build_content_event_at(
                author_secret,
                &Follow {
                    followee,
                    persona: None,
                    selector: right_selector.clone(),
                    persona_tags_selector: None,
                },
                timestamp,
            ),
        ];
        let expected_index = winning_event_index(&events);
        let expected_event_id = events[expected_index].event_id().to_short();
        let expected_unfollow_event_id = events
            .iter()
            .zip([left_selector.is_none(), right_selector.is_none()])
            .filter(|(_, is_unfollow)| *is_unfollow)
            .map(|(event, _)| event.event_id().to_short())
            .max();
        let expected_selector = [left_selector, right_selector][expected_index]
            .as_ref()
            .map(|selector| match selector {
                PersonaSelector::Only { ids } => PersonasTagsSelector::Only {
                    ids: ids
                        .iter()
                        .filter_map(|id| PersonaTag::from_persona_id(*id))
                        .collect(),
                },
                PersonaSelector::Except { ids } => PersonasTagsSelector::Except {
                    ids: ids
                        .iter()
                        .filter_map(|id| PersonaTag::from_persona_id(*id))
                        .collect(),
                },
            });

        for order in [[0, 1], [1, 0]] {
            let (_dir, db) = temp_db(author).await?;
            for index in order {
                db.process_event_with_content(&events[index]).await;
            }

            db.read_with(|tx| {
                let followees = tx.open_table(&ids_followees::TABLE)?;
                let followers = tx.open_table(&ids_followers::TABLE)?;
                let unfollowed = tx.open_table(&ids_unfollowed::TABLE)?;
                let singletons = tx.open_table(&events_singletons_new::TABLE)?;
                let follow_record = followees.get(&(author, followee))?.map(|g| g.value());

                if let Some(expected_selector) = expected_selector.as_ref() {
                    let follow_record = follow_record.expect("winning follow must remain active");
                    assert_eq!(follow_record.latest_event_id, expected_event_id);
                    assert_eq!(
                        follow_record.effective_tags_selector(),
                        expected_selector.clone()
                    );
                    assert!(followers.get(&(followee, author))?.is_some());
                    assert_eq!(
                        unfollowed
                            .get(&(author, followee))?
                            .map(|record| record.value().event_id),
                        expected_unfollow_event_id,
                    );
                } else {
                    assert!(follow_record.is_none());
                    assert!(followers.get(&(followee, author))?.is_none());
                    let unfollow_record = unfollowed
                        .get(&(author, followee))?
                        .map(|g| g.value())
                        .expect("winning unfollow must remain");
                    assert_eq!(unfollow_record.event_id, expected_event_id);
                }

                assert_eq!(
                    singletons
                        .get(&singleton_key)?
                        .map(|g| g.value().inner.event_id),
                    Some(expected_event_id)
                );
                Ok(())
            })
            .await?;
        }
    }

    Ok(())
}

/// Equal-second profile and generic singleton projections converge in both
/// orders.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_equal_timestamp_latest_values_converge() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind::{
        EventContentKind as _, SocialPost, SocialProfileUpdate,
    };

    use crate::{events_singletons_new, social_profiles};

    let author_secret = RostraIdSecretKey::generate();
    let author = author_secret.id();
    let timestamp = 20_000;
    let profiles = [
        build_content_event_at(
            author_secret,
            &SocialProfileUpdate {
                display_name: "Alice".to_owned(),
                bio: "first".to_owned(),
                avatar: None,
            },
            timestamp,
        ),
        build_content_event_at(
            author_secret,
            &SocialProfileUpdate {
                display_name: "Bob".to_owned(),
                bio: "second".to_owned(),
                avatar: None,
            },
            timestamp,
        ),
    ];
    let expected_profile_index = winning_event_index(&profiles);
    let expected_profile_id = profiles[expected_profile_index].event_id().to_short();
    let expected_profile_name = ["Alice", "Bob"][expected_profile_index];

    for order in [[0, 1], [1, 0]] {
        let (_dir, db) = temp_db(author).await?;
        for index in order {
            db.process_event_with_content(&profiles[index]).await;
        }

        let profile = db.get_social_profile(author).await.expect("profile");
        assert_eq!(profile.event_id, expected_profile_id);
        assert_eq!(profile.display_name, expected_profile_name);
        db.read_with(|tx| {
            let record = tx
                .open_table(&social_profiles::TABLE)?
                .get(&author)?
                .map(|g| g.value())
                .expect("raw profile");
            assert_eq!(record.ts, rostra_core::Timestamp::from(timestamp as u64));
            assert_eq!(record.inner.event_id, expected_profile_id);
            Ok(())
        })
        .await?;
    }

    let aux_key = EventAuxKey::from_bytes([7; 16]);
    let generic_events = [
        build_social_post_singleton_event_at(
            author_secret,
            aux_key,
            SocialPost::new_text("first".to_owned(), None, Default::default()).serialize_cbor()?,
            timestamp,
        ),
        build_social_post_singleton_event_at(
            author_secret,
            aux_key,
            SocialPost::new_text("second".to_owned(), None, Default::default()).serialize_cbor()?,
            timestamp,
        ),
    ];
    let expected_singleton_id = generic_events
        .iter()
        .map(|event| event.event_id().to_short())
        .max()
        .expect("two events");
    let singleton_key = (author, EventKind::SOCIAL_POST, aux_key);

    for order in [[0, 1], [1, 0]] {
        let (_dir, db) = temp_db(author).await?;
        for index in order {
            db.process_event_with_content(&generic_events[index]).await;
        }

        assert_eq!(
            db.get_latest_singleton_event(author, EventKind::SOCIAL_POST, aux_key)
                .await,
            Some(expected_singleton_id)
        );
        db.read_with(|tx| {
            let raw = tx
                .open_table(&events_singletons_new::TABLE)?
                .get(&singleton_key)?
                .map(|g| g.value())
                .expect("raw singleton");
            assert_eq!(raw.ts, rostra_core::Timestamp::from(timestamp as u64));
            assert_eq!(raw.inner.event_id, expected_singleton_id);
            Ok(())
        })
        .await?;
    }

    Ok(())
}

/// Equal-second up, down, and neutral vote pairs keep one winner and matching
/// sum.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_equal_timestamp_vote_conflicts_converge() -> BoxedErrorResult<()> {
    use crate::events_singletons_new;

    let voter_secret = RostraIdSecretKey::generate();
    let voter = voter_secret.id();
    let post_id = rostra_core::ExternalEventId::new(
        RostraIdSecretKey::generate().id(),
        rostra_core::ShortEventId::random(),
    );
    let timestamp = 30_000;

    for values in [
        [Some(true), Some(false)],
        [Some(true), None],
        [Some(false), None],
    ] {
        let events = values.map(|value| {
            build_content_event_at(
                voter_secret,
                &content_kind::SocialVote::new(post_id, value),
                timestamp,
            )
        });
        let expected_index = winning_event_index(&events);
        let expected_id = events[expected_index].event_id().to_short();
        let expected_vote = values[expected_index];
        let expected_sum = Database::social_vote_value(expected_vote);
        let singleton_key = (
            voter,
            EventKind::SOCIAL_VOTE,
            Database::social_vote_aux_key(post_id),
        );

        for order in [[0, 1], [1, 0]] {
            let (_dir, db) = temp_db(voter).await?;
            for index in order {
                db.process_event_with_content(&events[index]).await;
            }

            assert_eq!(
                db.get_social_vote(voter, post_id).await,
                Some(expected_vote)
            );
            assert_eq!(db.get_social_vote_sum(post_id).await, expected_sum);
            db.read_with(|tx| {
                let winner = tx
                    .open_table(&events_singletons_new::TABLE)?
                    .get(&singleton_key)?
                    .map(|g| g.value().inner.event_id);
                assert_eq!(winner, Some(expected_id));
                Ok(())
            })
            .await?;
        }
    }

    Ok(())
}

/// Shuffled finite singleton sets always select the maximum event order.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_shuffled_singleton_events_converge() -> BoxedErrorResult<()> {
    use rand::SeedableRng as _;
    use rand::seq::SliceRandom as _;
    use rostra_core::event::content_kind::{EventContentKind as _, SocialPost};

    let author_secret = RostraIdSecretKey::generate();
    let author = author_secret.id();
    let aux_key = EventAuxKey::from_bytes([9; 16]);
    let timestamps = [39_999, 40_000, 40_000, 39_998, 40_000];
    let events = timestamps
        .into_iter()
        .enumerate()
        .map(|(index, timestamp)| {
            build_social_post_singleton_event_at(
                author_secret,
                aux_key,
                SocialPost::new_text(format!("candidate {index}"), None, Default::default())
                    .serialize_cbor()
                    .expect("valid post"),
                timestamp,
            )
        })
        .collect::<Vec<_>>();
    let expected = events
        .iter()
        .max_by_key(|event| (event.timestamp(), event.event_id().to_short()))
        .expect("non-empty candidates")
        .event_id()
        .to_short();

    for seed in 0..32 {
        let (_dir, db) = temp_db(author).await?;
        let mut order = (0..events.len()).collect::<Vec<_>>();
        order.shuffle(&mut rand::rngs::StdRng::seed_from_u64(seed));
        for index in order {
            db.process_event_with_content(&events[index]).await;
        }
        assert_eq!(
            db.get_latest_singleton_event(author, EventKind::SOCIAL_POST, aux_key)
                .await,
            Some(expected)
        );
    }

    Ok(())
}
