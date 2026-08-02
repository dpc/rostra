use std::collections::BTreeSet;

use rand::SeedableRng as _;
use rand::seq::SliceRandom as _;
use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{
    Event, EventExt as _, EventKind, PersonaTag, PersonasTagsSelector, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;

use crate::{
    Database, DbError, IdsFolloweesRecord, db_version, ids_follow_events, ids_followees,
    ids_unfollowed, shoutbox_posts_by_received_at, social_posts_by_received_at,
};

#[derive(Debug, PartialEq, Eq)]
struct FollowSnapshot {
    latest_event_id: ShortEventId,
    first_ts: Timestamp,
    selector: PersonasTagsSelector,
    unfollow_boundary: Option<(Timestamp, ShortEventId)>,
    follow_events: Vec<(Timestamp, ShortEventId)>,
    effective_received_at: Vec<Timestamp>,
}

fn follow_event(
    secret: RostraIdSecretKey,
    followee: RostraId,
    timestamp: u64,
    selector_tag: Option<&str>,
    parent_marker: u8,
) -> VerifiedEventContent {
    let content = content_kind::Follow {
        followee,
        persona: None,
        selector: None,
        persona_tags_selector: selector_tag.map(|tag| PersonasTagsSelector::Only {
            ids: BTreeSet::from([PersonaTag::new(tag).expect("valid test tag")]),
        }),
    };
    let raw = content.serialize_cbor().expect("valid follow");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::FOLLOW)
        .parent_prev(ShortEventId::from_bytes([parent_marker; 16]))
        .content(&raw)
        .timestamp(
            time::OffsetDateTime::from_unix_timestamp(timestamp as i64)
                .expect("valid test timestamp"),
        )
        .build();
    let event =
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).expect("valid event");
    VerifiedEventContent::assume_verified(event, raw)
}

fn selector(tag: &str) -> PersonasTagsSelector {
    PersonasTagsSelector::Only {
        ids: BTreeSet::from([PersonaTag::new(tag).expect("valid test tag")]),
    }
}

fn shuffled_orders(len: usize, seed: u64) -> Vec<Vec<usize>> {
    let mut orders = vec![(0..len).collect(), (0..len).rev().collect()];
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    for _ in 0..24 {
        let mut order: Vec<_> = (0..len).collect();
        order.shuffle(&mut rng);
        orders.push(order);
    }
    orders
}

fn follow_orders(events: &[&VerifiedEventContent]) -> Vec<(Timestamp, ShortEventId)> {
    let mut orders: Vec<_> = events
        .iter()
        .map(|event| (event.timestamp(), event.event_id().to_short()))
        .collect();
    orders.sort_unstable();
    orders
}

async fn apply(db: &Database, events: &[VerifiedEventContent], order: &[usize]) {
    for &index in order {
        db.process_event_with_content(&events[index]).await;
    }
}

async fn snapshot(
    db: &Database,
    follower: RostraId,
    followee: RostraId,
    post_timestamps: &[u64],
    now: Timestamp,
) -> BoxedErrorResult<FollowSnapshot> {
    db.read_with(|tx| {
        let followees = tx.open_table(&ids_followees::TABLE)?;
        let record = followees
            .get(&(follower, followee))?
            .map(|guard| guard.value())
            .expect("relationship must be active");
        let unfollow_boundary = tx
            .open_table(&ids_unfollowed::TABLE)?
            .get(&(follower, followee))?
            .map(|guard| guard.value())
            .map(|record| (record.ts, record.event_id));
        let follow_events = tx
            .open_table(&ids_follow_events::TABLE)?
            .range(
                (follower, followee, Timestamp::ZERO, ShortEventId::ZERO)
                    ..=(follower, followee, Timestamp::MAX, ShortEventId::MAX),
            )?
            .map(|entry| {
                entry.map(|(key, _)| {
                    let (_, _, timestamp, event_id) = key.value();
                    (timestamp, event_id)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let effective_received_at = post_timestamps
            .iter()
            .map(|timestamp| {
                db.effective_received_at(followee, Timestamp::from(*timestamp), now, &followees)
            })
            .collect();

        Ok(FollowSnapshot {
            latest_event_id: record.latest_event_id,
            first_ts: record.first_ts,
            selector: record.effective_tags_selector(),
            unfollow_boundary,
            follow_events,
            effective_received_at,
        })
    })
    .await
    .map_err(Into::into)
}

async fn assert_scenario_converges(
    events: &[VerifiedEventContent],
    expected: &FollowSnapshot,
    follower: RostraId,
    followee: RostraId,
    post_timestamps: &[u64],
    seed: u64,
) -> BoxedErrorResult<()> {
    for order in shuffled_orders(events.len(), seed) {
        let db = Database::new_in_memory(follower).await?;
        apply(&db, events, &order).await;
        assert_eq!(
            snapshot(
                &db,
                follower,
                followee,
                post_timestamps,
                Timestamp::from(1_000),
            )
            .await?,
            *expected,
            "delivery order {order:?}",
        );
    }
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn follow_epochs_converge_across_zero_one_and_two_unfollows() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([91; 32]);
    let follower = secret.id();
    let followee = RostraIdSecretKey::from_bytes([92; 32]).id();

    let zero_epochs = vec![
        follow_event(secret, followee, 10, Some("zero-first"), 1),
        follow_event(secret, followee, 20, Some("zero-winner"), 2),
        follow_event(secret, followee, 15, Some("zero-middle"), 3),
    ];
    assert_scenario_converges(
        &zero_epochs,
        &FollowSnapshot {
            latest_event_id: zero_epochs[1].event_id().to_short(),
            first_ts: Timestamp::from(10),
            selector: selector("zero-winner"),
            unfollow_boundary: None,
            follow_events: follow_orders(&[&zero_epochs[0], &zero_epochs[1], &zero_epochs[2]]),
            effective_received_at: vec![
                Timestamp::from(9),
                Timestamp::from(1_000),
                Timestamp::from(1_000),
            ],
        },
        follower,
        followee,
        &[9, 10, 15],
        10,
    )
    .await?;

    let one_epoch = vec![
        follow_event(secret, followee, 10, Some("one-old"), 4),
        follow_event(secret, followee, 18, Some("one-old-late"), 5),
        follow_event(secret, followee, 20, None, 6),
        follow_event(secret, followee, 30, Some("one-new"), 7),
        follow_event(secret, followee, 25, Some("one-first"), 8),
        follow_event(secret, followee, 40, Some("one-winner"), 9),
    ];
    assert_scenario_converges(
        &one_epoch,
        &FollowSnapshot {
            latest_event_id: one_epoch[5].event_id().to_short(),
            first_ts: Timestamp::from(25),
            selector: selector("one-winner"),
            unfollow_boundary: Some((Timestamp::from(20), one_epoch[2].event_id().to_short())),
            follow_events: follow_orders(&[&one_epoch[3], &one_epoch[4], &one_epoch[5]]),
            effective_received_at: vec![
                Timestamp::from(19),
                Timestamp::from(20),
                Timestamp::from(24),
                Timestamp::from(1_000),
                Timestamp::from(1_000),
            ],
        },
        follower,
        followee,
        &[19, 20, 24, 25, 35],
        20,
    )
    .await?;

    let two_epochs = vec![
        follow_event(secret, followee, 10, Some("two-oldest"), 10),
        follow_event(secret, followee, 20, None, 11),
        follow_event(secret, followee, 30, Some("two-middle"), 12),
        follow_event(secret, followee, 35, Some("two-middle-late"), 13),
        follow_event(secret, followee, 40, None, 14),
        follow_event(secret, followee, 50, Some("two-new"), 15),
        follow_event(secret, followee, 45, Some("two-first"), 16),
        follow_event(secret, followee, 60, Some("two-winner"), 17),
    ];
    assert_scenario_converges(
        &two_epochs,
        &FollowSnapshot {
            latest_event_id: two_epochs[7].event_id().to_short(),
            first_ts: Timestamp::from(45),
            selector: selector("two-winner"),
            unfollow_boundary: Some((Timestamp::from(40), two_epochs[4].event_id().to_short())),
            follow_events: follow_orders(&[&two_epochs[5], &two_epochs[6], &two_epochs[7]]),
            effective_received_at: vec![
                Timestamp::from(15),
                Timestamp::from(25),
                Timestamp::from(39),
                Timestamp::from(40),
                Timestamp::from(44),
                Timestamp::from(1_000),
                Timestamp::from(1_000),
            ],
        },
        follower,
        followee,
        &[15, 25, 39, 40, 44, 45, 55],
        30,
    )
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn concentrated_follow_history_prunes_in_bounded_batches() -> BoxedErrorResult<()> {
    const HISTORY_LEN: usize = 600;

    let secret = RostraIdSecretKey::from_bytes([0xb1; 32]);
    let follower = secret.id();
    let followee = RostraIdSecretKey::from_bytes([0xb2; 32]).id();
    let db = Database::new_in_memory(follower).await?;
    let follows = (0..HISTORY_LEN)
        .map(|index| follow_event(secret, followee, index as u64 + 1, Some("bulk"), 1))
        .collect::<Vec<_>>();
    let unfollow = follow_event(secret, followee, 1_000, None, 2);

    db.write_with(|tx| {
        for event in follows.iter().chain(std::iter::once(&unfollow)) {
            db.process_event_tx(&event.event, event.timestamp(), tx)?;
            db.process_event_content_tx(event, event.timestamp(), tx)?;
        }
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let history = tx.open_table(&ids_follow_events::TABLE)?;
        assert!(
            history
                .range(
                    (follower, followee, Timestamp::ZERO, ShortEventId::ZERO)
                        ..=(follower, followee, Timestamp::MAX, ShortEventId::MAX)
                )?
                .next()
                .is_none(),
            "all {HISTORY_LEN} obsolete rows must be removed across multiple 256-key batches"
        );
        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn equal_second_event_order_defines_epoch_membership() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([93; 32]);
    let follower = secret.id();
    let followee = RostraIdSecretKey::from_bytes([94; 32]).id();
    let follows: Vec<_> = (1..=64)
        .map(|marker| follow_event(secret, followee, 100, Some("same-second"), marker))
        .collect();
    let unfollows: Vec<_> = (65..=128)
        .map(|marker| follow_event(secret, followee, 100, None, marker))
        .collect();
    let (unfollow, before, after) = unfollows
        .iter()
        .find_map(|unfollow| {
            let boundary = unfollow.event_id().to_short();
            Some((
                unfollow.clone(),
                follows
                    .iter()
                    .filter(|follow| follow.event_id().to_short() < boundary)
                    .max_by_key(|follow| follow.event_id().to_short())?
                    .clone(),
                follows
                    .iter()
                    .filter(|follow| boundary < follow.event_id().to_short())
                    .min_by_key(|follow| follow.event_id().to_short())?
                    .clone(),
            ))
        })
        .expect("generated follows straddle an unfollow");
    let events = vec![before, unfollow.clone(), after.clone()];
    let expected = FollowSnapshot {
        latest_event_id: after.event_id().to_short(),
        first_ts: Timestamp::from(100),
        selector: selector("same-second"),
        unfollow_boundary: Some((Timestamp::from(100), unfollow.event_id().to_short())),
        follow_events: follow_orders(&[&after]),
        effective_received_at: vec![Timestamp::from(99), Timestamp::from(1_000)],
    };

    assert_scenario_converges(&events, &expected, follower, followee, &[99, 100], 40).await
}

fn social_post(secret: RostraIdSecretKey, timestamp: u64) -> VerifiedEventContent {
    let content = content_kind::SocialPost::new("post".to_owned(), None, Default::default())
        .serialize_cbor()
        .expect("valid social post");
    content_event(secret, EventKind::SOCIAL_POST, content, timestamp)
}

fn shoutbox_post(secret: RostraIdSecretKey, timestamp: u64) -> VerifiedEventContent {
    let content = content_kind::Shoutbox {
        djot_content: "shout".to_owned(),
    }
    .serialize_cbor()
    .expect("valid shoutbox post");
    content_event(secret, EventKind::SHOUTBOX, content, timestamp)
}

fn content_event(
    secret: RostraIdSecretKey,
    kind: EventKind,
    content: rostra_core::event::EventContentRaw,
    timestamp: u64,
) -> VerifiedEventContent {
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(kind)
        .content(&content)
        .timestamp(
            time::OffsetDateTime::from_unix_timestamp(timestamp as i64).expect("valid timestamp"),
        )
        .build();
    let event =
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).expect("valid event");
    VerifiedEventContent::assume_verified(event, content)
}

async fn process_at(
    db: &Database,
    content: &VerifiedEventContent,
    now: Timestamp,
) -> BoxedErrorResult<()> {
    db.write_with(|tx| {
        db.process_event_tx(&content.event, now, tx)?;
        db.process_event_content_tx(content, now, tx)
    })
    .await
    .boxed()?;
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn post_and_shout_notifications_use_current_epoch_cutoff() -> BoxedErrorResult<()> {
    let follower_secret = RostraIdSecretKey::from_bytes([95; 32]);
    let author_secret = RostraIdSecretKey::from_bytes([96; 32]);
    let db = Database::new_in_memory(follower_secret.id()).await?;
    let follow_events = [
        follow_event(follower_secret, author_secret.id(), 10, Some("old"), 1),
        follow_event(follower_secret, author_secret.id(), 20, None, 2),
        follow_event(follower_secret, author_secret.id(), 30, Some("new"), 3),
        follow_event(follower_secret, author_secret.id(), 25, Some("first"), 4),
    ];
    apply(&db, &follow_events, &[3, 0, 2, 1]).await;

    let old_post = social_post(author_secret, 24);
    let current_post = social_post(author_secret, 25);
    let old_shout = shoutbox_post(author_secret, 24);
    let current_shout = shoutbox_post(author_secret, 25);
    let now = Timestamp::from(1_000);
    for content in [&old_post, &current_post, &old_shout, &current_shout] {
        process_at(&db, content, now).await?;
    }

    db.read_with(|tx| {
        let social = tx.open_table(&social_posts_by_received_at::TABLE)?;
        assert!(
            social
                .range((Timestamp::from(24), 0)..=(Timestamp::from(24), u64::MAX),)?
                .any(|entry| entry.is_ok())
        );
        assert!(
            social
                .range((now, 0)..=(now, u64::MAX))?
                .any(|entry| entry.is_ok())
        );
        let shoutbox = tx.open_table(&shoutbox_posts_by_received_at::TABLE)?;
        assert!(
            shoutbox
                .range((Timestamp::from(24), 0)..=(Timestamp::from(24), u64::MAX),)?
                .any(|entry| entry.is_ok())
        );
        assert!(
            shoutbox
                .range((now, 0)..=(now, u64::MAX))?
                .any(|entry| entry.is_ok())
        );
        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn metadata_only_epoch_changes_publish_followee_state() -> BoxedErrorResult<()> {
    let follower_secret = RostraIdSecretKey::from_bytes([99; 32]);
    let follower = follower_secret.id();
    let followee = RostraIdSecretKey::from_bytes([100; 32]).id();
    let winner = follow_event(follower_secret, followee, 30, Some("winner"), 1);
    let late_follow = follow_event(follower_secret, followee, 20, Some("older"), 2);
    let late_unfollow = follow_event(follower_secret, followee, 25, None, 3);
    let db = Database::new_in_memory(follower).await?;

    db.process_event_with_content(&winner).await;
    let mut followees = db.self_followees_subscribe();
    let initial = followees
        .snapshot()
        .get(&followee)
        .expect("active follow")
        .clone();
    assert_eq!(initial.latest_event_id, winner.event_id().to_short());
    assert_eq!(initial.first_ts, Timestamp::from(30));

    db.process_event_with_content(&late_follow).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), followees.changed())
        .await
        .expect("late follow publication")
        .expect("watch remains open");
    {
        let updated = followees.snapshot();
        let record = updated.get(&followee).expect("follow remains active");
        assert_eq!(record.latest_event_id, winner.event_id().to_short());
        assert_eq!(record.first_ts, Timestamp::from(20));
    }

    db.process_event_with_content(&late_unfollow).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), followees.changed())
        .await
        .expect("late unfollow publication")
        .expect("watch remains open");
    let updated = followees.snapshot();
    let record = updated.get(&followee).expect("follow remains active");
    assert_eq!(record.latest_event_id, winner.event_id().to_short());
    assert_eq!(record.first_ts, Timestamp::from(30));

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn public_self_follow_snapshot_tracks_uninterrupted_epochs() -> BoxedErrorResult<()> {
    let follower_secret = RostraIdSecretKey::from_bytes([101; 32]);
    let follower = follower_secret.id();
    let followee = RostraIdSecretKey::from_bytes([102; 32]).id();
    let first = follow_event(follower_secret, followee, 10, Some("old"), 1);
    let unfollow = follow_event(follower_secret, followee, 20, None, 2);
    let refollow = follow_event(follower_secret, followee, 30, Some("winner"), 3);
    let earlier_current = follow_event(follower_secret, followee, 25, Some("earlier"), 4);
    let db = Database::new_in_memory(follower).await?;

    db.process_event_with_content(&first).await;
    assert_eq!(
        db.get_self_followees_snapshot().await?,
        [crate::SelfFollowee {
            followee,
            persona_selector: selector("old"),
            first_ts: Timestamp::from(10),
        }]
    );

    db.process_event_with_content(&unfollow).await;
    assert!(db.get_self_followees_snapshot().await?.is_empty());

    db.process_event_with_content(&refollow).await;
    assert_eq!(
        db.get_self_followees_snapshot().await?,
        [crate::SelfFollowee {
            followee,
            persona_selector: selector("winner"),
            first_ts: Timestamp::from(30),
        }]
    );

    db.process_event_with_content(&earlier_current).await;
    assert_eq!(
        db.get_self_followees_snapshot().await?,
        [crate::SelfFollowee {
            followee,
            persona_selector: selector("winner"),
            first_ts: Timestamp::from(25),
        }]
    );
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn public_self_follow_snapshot_fails_on_owned_corruption_only() -> BoxedErrorResult<()> {
    let owner = RostraIdSecretKey::from_bytes([103; 32]).id();
    let followee = RostraIdSecretKey::from_bytes([104; 32]).id();
    let adjacent_owner = RostraIdSecretKey::from_bytes([105; 32]).id();
    let prefix = bincode::encode_to_vec(owner, redb_bincode::BINCODE_CONFIG)?;
    let key = bincode::encode_to_vec((owner, followee), redb_bincode::BINCODE_CONFIG)?;
    let adjacent_key =
        bincode::encode_to_vec((adjacent_owner, followee), redb_bincode::BINCODE_CONFIG)?;
    let record = IdsFolloweesRecord::new(
        Timestamp::from(10),
        ShortEventId::ZERO,
        Timestamp::from(10),
        None,
        None,
    );
    let value = bincode::encode_to_vec(record, redb_bincode::BINCODE_CONFIG)?;

    let mut corruptions = vec![
        (prefix.clone(), value.clone()),
        ([key.as_slice(), &[0]].concat(), value.clone()),
        (key.clone(), vec![0xff]),
        (key.clone(), [value.as_slice(), &[0]].concat()),
    ];
    for (corrupt_key, corrupt_value) in corruptions.drain(..) {
        let db = Database::new_in_memory(owner).await?;
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(ids_followees::TABLE.as_raw())?
                .insert(corrupt_key.as_slice(), corrupt_value.as_slice())?;
            Ok(())
        })
        .await?;
        assert!(matches!(
            db.get_self_followees_snapshot().await,
            Err(DbError::StoredDecode { .. })
        ));
    }

    let db = Database::new_in_memory(owner).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(ids_followees::TABLE.as_raw())?
            .insert(adjacent_key.as_slice(), &[0xff][..])?;
        Ok(())
    })
    .await?;
    assert!(db.get_self_followees_snapshot().await?.is_empty());
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn follow_epoch_survives_reopen_and_total_replay() -> BoxedErrorResult<()> {
    let follower_secret = RostraIdSecretKey::from_bytes([97; 32]);
    let follower = follower_secret.id();
    let followee = RostraIdSecretKey::from_bytes([98; 32]).id();
    let events = vec![
        follow_event(follower_secret, followee, 10, Some("old"), 1),
        follow_event(follower_secret, followee, 20, None, 2),
        follow_event(follower_secret, followee, 30, Some("new"), 3),
        follow_event(follower_secret, followee, 25, Some("first"), 4),
        follow_event(follower_secret, followee, 40, Some("winner"), 5),
    ];
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("follow-epoch.redb");

    {
        let db = Database::open(&path, follower).await?;
        apply(&db, &events, &[4, 0, 2, 1, 3]).await;
    }

    let expected = {
        let db = Database::open(&path, follower).await?;
        snapshot(
            &db,
            follower,
            followee,
            &[19, 20, 24, 25, 35],
            Timestamp::from(1_000),
        )
        .await?
    };

    {
        let raw_db = redb_bincode::Database::from(redb::Database::open(&path).boxed()?);
        let write_txn = raw_db.begin_write().boxed()?;
        write_txn
            .open_table(&db_version::TABLE)
            .boxed()?
            .insert(&(), &24)
            .boxed()?;
        write_txn.commit().boxed()?;
    }

    let replayed = Database::open(&path, follower).await?;
    assert_eq!(
        snapshot(
            &replayed,
            follower,
            followee,
            &[19, 20, 24, 25, 35],
            Timestamp::from(1_000),
        )
        .await?,
        expected,
    );

    Ok(())
}
