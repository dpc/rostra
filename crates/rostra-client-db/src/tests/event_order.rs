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

fn build_social_vote_event_at(
    id_secret: RostraIdSecretKey,
    vote: &content_kind::SocialVote,
    timestamp: i64,
    singleton_aux: Option<EventAuxKey>,
) -> VerifiedEventContent {
    use rostra_core::event::content_kind::EventContentKind as _;

    let author = id_secret.id();
    let content = vote.serialize_cbor().expect("valid vote");
    let builder = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_VOTE)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .content(&content);
    let event = if let Some(aux_key) = singleton_aux {
        builder.singleton_aux_key(aux_key).build()
    } else {
        builder.build()
    };
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
        let expected_sum = match expected_vote {
            Some(true) => 1,
            None => 0,
            Some(false) => -1,
        };
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

/// Winner and aggregate updates roll back together when their transaction
/// aborts.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn social_vote_winner_and_sum_update_atomically() -> BoxedErrorResult<()> {
    use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

    use crate::events_singletons_new;

    let voter_secret = RostraIdSecretKey::generate();
    let voter = voter_secret.id();
    let shared_post_id = ShortEventId::random();
    let old_post = ExternalEventId::new(RostraIdSecretKey::generate().id(), shared_post_id);
    let replacement_post = ExternalEventId::new(RostraIdSecretKey::generate().id(), shared_post_id);
    let (_dir, db) = temp_db(voter).await?;

    let old_vote = build_content_event_at(
        voter_secret,
        &content_kind::SocialVote::new(old_post, Some(true)),
        50_000,
    );
    let old_id = old_vote.event_id().to_short();
    db.process_event_with_content(&old_vote).await;

    let replacement = build_content_event_at(
        voter_secret,
        &content_kind::SocialVote::new(replacement_post, Some(false)),
        50_001,
    );
    let replacement_id = replacement.event_id().to_short();
    let error = db
        .write_with(|tx| {
            db.process_event_tx(&replacement.event, Timestamp::from(50_002), tx)?;
            db.process_event_content_tx(&replacement, Timestamp::from(50_002), tx)?;
            crate::OverflowSnafu.fail::<()>()
        })
        .await
        .expect_err("injected failure must abort vote update");
    assert!(matches!(error, crate::DbError::Overflow));

    assert!(!db.has_event(replacement_id).await);
    assert_eq!(db.get_social_vote(voter, old_post).await, Some(Some(true)));
    assert_eq!(db.get_social_vote(voter, replacement_post).await, None);
    assert_eq!(db.get_social_vote_sum(old_post).await, 1);
    assert_eq!(db.get_social_vote_sum(replacement_post).await, 0);
    db.read_with(|tx| {
        let winner = tx
            .open_table(&events_singletons_new::TABLE)?
            .get(&(
                voter,
                EventKind::SOCIAL_VOTE,
                Database::social_vote_aux_key(old_post),
            ))?
            .map(|record| record.value().inner.event_id);
        assert_eq!(winner, Some(old_id));
        Ok(())
    })
    .await?;

    Ok(())
}

/// Full vote targets that share one shortened event ID transfer one winner's
/// contribution deterministically across both aggregates and total replay.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn colliding_full_vote_targets_converge_and_replay() -> BoxedErrorResult<()> {
    use bincode::{Decode, Encode};
    use rostra_core::{ExternalEventId, ShortEventId};

    #[derive(Encode, Decode)]
    struct Version24EventSingletonRecord {
        event_id: ShortEventId,
    }

    let voter_secret = RostraIdSecretKey::generate();
    let voter = voter_secret.id();
    let shared_post_id = ShortEventId::random();
    let post_a = ExternalEventId::new(RostraIdSecretKey::generate().id(), shared_post_id);
    let post_b = ExternalEventId::new(RostraIdSecretKey::generate().id(), shared_post_id);
    let events = [
        build_content_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_a, Some(true)),
            50_100,
        ),
        build_content_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_b, Some(true)),
            50_101,
        ),
        build_content_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_a, Some(false)),
            50_102,
        ),
        build_content_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_b, None),
            50_103,
        ),
    ];

    for order in [[0, 1, 2, 3], [3, 2, 1, 0]] {
        let (dir, db) = temp_db(voter).await?;
        for index in order {
            db.process_event_with_content(&events[index]).await;
        }
        assert_eq!(db.get_social_vote(voter, post_a).await, None);
        assert_eq!(db.get_social_vote(voter, post_b).await, Some(None));
        assert_eq!(db.get_social_vote_sum(post_a).await, 0);
        assert_eq!(db.get_social_vote_sum(post_b).await, 0);

        drop(db);
        let db_path = dir.path().join("db.redb");
        let raw_db = redb_bincode::Database::from(redb::Database::open(&db_path)?);
        let write_txn = raw_db.begin_write()?;
        {
            // qsc3 added an inline full-target/value projection. Install the
            // actual pre-qsc3 v24 encoding so open succeeds only when migration
            // discards the incompatible derived row before decoding it.
            assert!(
                write_txn
                    .as_raw()
                    .delete_table(crate::events_singletons_new::TABLE.as_raw())?
            );
            let legacy_singletons: redb_bincode::TableDefinition<
                '_,
                (
                    rostra_core::id::RostraId,
                    EventKind,
                    rostra_core::event::EventAuxKey,
                ),
                crate::Latest<Version24EventSingletonRecord>,
            > = redb_bincode::TableDefinition::new("events_singletons_new");
            write_txn.open_table(&legacy_singletons)?.insert(
                &(
                    voter,
                    EventKind::SOCIAL_VOTE,
                    rostra_core::event::EventAuxKey::from_bytes(shared_post_id.to_bytes()),
                ),
                &crate::Latest {
                    ts: events[3].timestamp(),
                    inner: Version24EventSingletonRecord {
                        event_id: events[3].event_id().to_short(),
                    },
                },
            )?;
            let mut version = write_txn.open_table(&crate::db_version::TABLE)?;
            version.insert(&(), &24)?;
        }
        write_txn.commit()?;
        drop(raw_db);

        let replayed = Database::open(&db_path, voter).await?;
        assert_eq!(replayed.get_social_vote(voter, post_a).await, None);
        assert_eq!(replayed.get_social_vote(voter, post_b).await, Some(None));
        assert_eq!(replayed.get_social_vote_sum(post_a).await, 0);
        assert_eq!(replayed.get_social_vote_sum(post_b).await, 0);
    }

    Ok(())
}

/// A mismatched vote payload does not enter the coupled winner/aggregate
/// projection or block a correctly shaped older vote.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn malformed_vote_shape_does_not_poison_projection() -> BoxedErrorResult<()> {
    use rostra_core::{ExternalEventId, ShortEventId};

    let voter_secret = RostraIdSecretKey::generate();
    let voter = voter_secret.id();
    let post_id = ExternalEventId::new(
        RostraIdSecretKey::generate().id(),
        ShortEventId::from_bytes([0x11; 16]),
    );
    let (_dir, db) = temp_db(voter).await?;

    let malformed = [
        build_social_vote_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_id, Some(true)),
            51_003,
            Some(EventAuxKey::from_bytes([0x55; 16])),
        ),
        build_social_vote_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_id, Some(true)),
            51_002,
            None,
        ),
        build_social_vote_event_at(
            voter_secret,
            &content_kind::SocialVote {
                reply_to: None,
                upvote: Some(true),
            },
            51_001,
            Some(Database::social_vote_aux_key(post_id)),
        ),
    ];
    for event in malformed {
        db.process_event_with_content(&event).await;
    }
    assert_eq!(db.get_social_vote(voter, post_id).await, None);
    assert_eq!(db.get_social_vote_sum(post_id).await, 0);
    db.read_with(|tx| {
        let vote_winners = tx
            .open_table(&crate::events_singletons_new::TABLE)?
            .range(..)?
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|(key, _)| key.value().1 == EventKind::SOCIAL_VOTE)
            })
            .count();
        assert_eq!(vote_winners, 0);
        Ok(())
    })
    .await?;

    let valid = build_content_event_at(
        voter_secret,
        &content_kind::SocialVote::new(post_id, Some(false)),
        51_000,
    );
    db.process_event_with_content(&valid).await;
    assert_eq!(db.get_social_vote(voter, post_id).await, Some(Some(false)));
    assert_eq!(db.get_social_vote_sum(post_id).await, -1);

    Ok(())
}

/// A vote winner with missing or key-inconsistent cached state fails closed on
/// reads and replacement without changing its aggregate or candidate event.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn invalid_cached_vote_projection_fails_closed() -> BoxedErrorResult<()> {
    use futures::FutureExt as _;
    use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

    #[derive(Clone, Copy, Debug)]
    enum Corruption {
        Missing,
        WrongShortTarget,
    }

    for corruption in [Corruption::Missing, Corruption::WrongShortTarget] {
        let voter_secret = RostraIdSecretKey::generate();
        let voter = voter_secret.id();
        let post_id = ExternalEventId::new(
            RostraIdSecretKey::generate().id(),
            ShortEventId::from_bytes([0x66; 16]),
        );
        let wrong_target = ExternalEventId::new(
            RostraIdSecretKey::generate().id(),
            ShortEventId::from_bytes([0x77; 16]),
        );
        let (_dir, db) = temp_db(voter).await?;
        let singleton_key = (
            voter,
            EventKind::SOCIAL_VOTE,
            Database::social_vote_aux_key(post_id),
        );

        let old_vote = build_content_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_id, Some(true)),
            52_000,
        );
        let old_id = old_vote.event_id().to_short();
        db.process_event_with_content(&old_vote).await;
        let corrupt_winner = db
            .write_with(|tx| {
                let mut table = tx.open_table(&crate::events_singletons_new::TABLE)?;
                let mut winner = table
                    .get(&singleton_key)?
                    .expect("vote winner exists")
                    .value();
                match corruption {
                    Corruption::Missing => winner.inner.social_vote = None,
                    Corruption::WrongShortTarget => {
                        winner
                            .inner
                            .social_vote
                            .as_mut()
                            .expect("valid winner has a projection")
                            .target = wrong_target;
                    }
                }
                table.insert(&singleton_key, &winner)?;
                Ok(winner)
            })
            .await?;

        let read = std::panic::AssertUnwindSafe(db.get_social_vote(voter, post_id))
            .catch_unwind()
            .await;
        assert!(
            read.is_err(),
            "{corruption:?}: corrupt cached projection must fail the read"
        );

        let replacement = build_content_event_at(
            voter_secret,
            &content_kind::SocialVote::new(post_id, Some(false)),
            52_001,
        );
        let replacement_id = replacement.event_id().to_short();
        let error = db
            .write_with(|tx| {
                db.process_event_tx(&replacement.event, Timestamp::from(52_002), tx)?;
                db.process_event_content_tx(&replacement, Timestamp::from(52_002), tx)
            })
            .await
            .expect_err("invalid cached projection must abort replacement");
        assert!(
            match corruption {
                Corruption::Missing => matches!(
                    error,
                    crate::DbError::MissingVoteSingletonProjection { event_id, .. }
                        if event_id == old_id
                ),
                Corruption::WrongShortTarget => matches!(
                    error,
                    crate::DbError::InvalidVoteSingletonProjection { event_id, .. }
                        if event_id == old_id
                ),
            },
            "{corruption:?}: {error:?}"
        );
        assert!(!db.has_event(replacement_id).await, "{corruption:?}");
        assert_eq!(db.get_social_vote_sum(post_id).await, 1, "{corruption:?}");
        assert_eq!(
            db.get_social_vote_sum(wrong_target).await,
            0,
            "{corruption:?}"
        );
        db.read_with(|tx| {
            let winner = tx
                .open_table(&crate::events_singletons_new::TABLE)?
                .get(&singleton_key)?
                .expect("old winner remains")
                .value();
            assert_eq!(winner.ts, corrupt_winner.ts, "{corruption:?}");
            assert_eq!(winner.inner.event_id, old_id, "{corruption:?}");
            assert_eq!(
                winner.inner.social_vote, corrupt_winner.inner.social_vote,
                "{corruption:?}"
            );
            Ok(())
        })
        .await?;
    }

    Ok(())
}

/// A retained vote remains readable and replaceable after its source becomes
/// legitimately unavailable through delete and content garbage collection.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn vote_winner_survives_deleted_and_collected_source() -> BoxedErrorResult<()> {
    use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

    use crate::event::EventContentState;
    use crate::{content_store, events_content_state};

    let voter_secret = RostraIdSecretKey::generate();
    let voter = voter_secret.id();
    let post_id = ExternalEventId::new(RostraIdSecretKey::generate().id(), ShortEventId::random());
    let (dir, db) = temp_db(voter).await?;

    let old_vote = build_content_event_at(
        voter_secret,
        &content_kind::SocialVote::new(post_id, Some(true)),
        55_000,
    );
    let old_content_hash = old_vote.content_hash();
    db.process_event_with_content(&old_vote).await;
    assert_eq!(db.get_social_vote(voter, post_id).await, Some(Some(true)));
    assert_eq!(db.get_social_vote_sum(post_id).await, 1);

    let delete = super::build_delete_event(voter_secret, old_vote.event_id(), old_vote.event_id());
    db.process_event(&delete).await;
    db.read_with(|tx| {
        assert!(matches!(
            Database::get_event_content_state_tx(
                old_vote.event_id().to_short(),
                &tx.open_table(&events_content_state::TABLE)?,
            )?,
            Some(EventContentState::Deleted { .. })
        ));
        Ok(())
    })
    .await?;

    db.write_with(|tx| {
        assert!(
            tx.open_table(&content_store::TABLE)?
                .remove(&old_content_hash)?
                .is_some()
        );
        Ok(())
    })
    .await?;
    assert_eq!(db.get_social_vote(voter, post_id).await, Some(Some(true)));

    let replacement = build_content_event_at(
        voter_secret,
        &content_kind::SocialVote::new(post_id, Some(false)),
        55_001,
    );
    db.write_with(|tx| {
        db.process_event_tx(&replacement.event, Timestamp::from(55_002), tx)?;
        db.process_event_content_tx(&replacement, Timestamp::from(55_002), tx)
    })
    .await?;

    assert_eq!(db.get_social_vote(voter, post_id).await, Some(Some(false)));
    assert_eq!(db.get_social_vote_sum(post_id).await, -1);
    assert!(db.has_event(replacement.event_id().to_short()).await);

    drop(db);
    let db_path = dir.path().join("db.redb");
    let raw_db = redb_bincode::Database::from(redb::Database::open(&db_path)?);
    let write_txn = raw_db.begin_write()?;
    {
        let mut version = write_txn.open_table(&crate::db_version::TABLE)?;
        version.insert(&(), &24)?;
    }
    write_txn.commit()?;
    drop(raw_db);

    let replayed = Database::open(&db_path, voter).await?;
    assert_eq!(
        replayed.get_social_vote(voter, post_id).await,
        Some(Some(false))
    );
    assert_eq!(replayed.get_social_vote_sum(post_id).await, -1);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn latest_singleton_query_is_isolated_ordered_and_strict() -> BoxedErrorResult<()> {
    use futures::FutureExt as _;
    use rostra_core::Timestamp;

    use crate::event::EventSingletonRecord;
    use crate::{Latest, events_singletons_new};

    let author_secret = RostraIdSecretKey::generate();
    let author = author_secret.id();
    let other_author = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(author).await?;
    let first = rostra_core::ShortEventId::random();
    let second = rostra_core::ShortEventId::random();
    let newest = rostra_core::ShortEventId::random();
    let (lower_tie, higher_tie) = if first < second {
        (first, second)
    } else {
        (second, first)
    };

    db.write_with(|tx| {
        let mut table = tx.open_table(&events_singletons_new::TABLE)?;
        for (key_author, kind, aux, timestamp, event_id) in [
            (
                author,
                EventKind::SOCIAL_MEDIA,
                EventAuxKey::from_bytes([1; 16]),
                60_000,
                lower_tie,
            ),
            (
                author,
                EventKind::SOCIAL_MEDIA,
                EventAuxKey::from_bytes([2; 16]),
                60_000,
                higher_tie,
            ),
            (
                author,
                EventKind::SOCIAL_MEDIA,
                EventAuxKey::from_bytes([3; 16]),
                60_001,
                newest,
            ),
            (
                other_author,
                EventKind::SOCIAL_MEDIA,
                EventAuxKey::from_bytes([4; 16]),
                70_000,
                rostra_core::ShortEventId::random(),
            ),
            (
                author,
                EventKind::SOCIAL_PROFILE_UPDATE,
                EventAuxKey::from_bytes([5; 16]),
                70_000,
                rostra_core::ShortEventId::random(),
            ),
        ] {
            table.insert(
                &(key_author, kind, aux),
                &Latest {
                    ts: Timestamp::from(timestamp),
                    inner: EventSingletonRecord {
                        event_id,
                        social_vote: None,
                    },
                },
            )?;
        }
        Ok(())
    })
    .await?;

    assert_eq!(
        db.get_latest_singleton_events(author, EventKind::SOCIAL_MEDIA)
            .await,
        vec![newest, higher_tie, lower_tie]
    );

    db.write_with(|tx| {
        let key = (
            author,
            EventKind::SOCIAL_MEDIA,
            EventAuxKey::from_bytes([4; 16]),
        );
        let value = Latest {
            ts: Timestamp::from(60_002),
            inner: EventSingletonRecord {
                event_id: rostra_core::ShortEventId::random(),
                social_vote: None,
            },
        };
        let mut raw_key = bincode::encode_to_vec(key, redb_bincode::BINCODE_CONFIG)
            .expect("key encoding succeeds");
        raw_key.push(0);
        let raw_value = bincode::encode_to_vec(value, redb_bincode::BINCODE_CONFIG)
            .expect("value encoding succeeds");
        tx.as_raw()
            .open_table(events_singletons_new::TABLE.as_raw())?
            .insert(raw_key.as_slice(), raw_value.as_slice())?;
        Ok(())
    })
    .await?;

    let malformed = std::panic::AssertUnwindSafe(
        db.get_latest_singleton_events(author, EventKind::SOCIAL_MEDIA),
    )
    .catch_unwind()
    .await;
    assert!(malformed.is_err());

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
