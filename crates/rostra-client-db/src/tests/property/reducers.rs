use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use rostra_core::event::content_kind::{self, Follow, SocialProfileUpdate, SocialVote};
use rostra_core::event::{
    Event, EventAuxKey, EventContentRaw, EventExt as _, EventKind, PersonaId, PersonaSelector,
    PersonaTag, PersonasTagsSelector, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

use super::runner::{Plan, plan_strategy, run_pair, run_property, with_content};
use crate::{
    Database, DbResult, events_singletons_new, ids_follow_events, ids_followees, ids_followers,
    ids_unfollowed, social_profiles, social_vote_sums,
};

fn signed_content<C: content_kind::EventContentKind>(
    secret: RostraIdSecretKey,
    content: &C,
    timestamp: u64,
) -> VerifiedEventContent {
    let author = secret.id();
    let (event, raw) = Event::builder(content)
        .author(author)
        .timestamp(
            time::OffsetDateTime::from_unix_timestamp(timestamp as i64)
                .expect("property timestamp"),
        )
        .build()
        .expect("valid reducer content");
    let event = VerifiedEvent::verify_signed(author, event.signed_by(secret))
        .expect("deterministic event verifies");
    with_content(event, raw)
}

fn order(event: &VerifiedEventContent) -> (Timestamp, ShortEventId) {
    (event.timestamp(), event.event_id().to_short())
}

#[derive(Clone, Debug)]
struct FollowSpec {
    author: u8,
    followee: u8,
    timestamp: u8,
    selector: u8,
}

struct FollowInput {
    event: VerifiedEventContent,
    author: RostraId,
    followee: RostraId,
    selector: Option<PersonasTagsSelector>,
}

#[derive(Debug, PartialEq, Eq)]
struct ActiveFollow {
    latest_ts: Timestamp,
    latest_event_id: ShortEventId,
    first_ts: Timestamp,
    selector: PersonasTagsSelector,
}

#[derive(Debug, PartialEq, Eq)]
struct FollowSnapshot {
    active: BTreeMap<(RostraId, RostraId), ActiveFollow>,
    reverse: BTreeSet<(RostraId, RostraId)>,
    winners: BTreeMap<(RostraId, [u8; 16]), (Timestamp, ShortEventId)>,
    unfollowed: BTreeMap<(RostraId, RostraId), (Timestamp, ShortEventId)>,
    follow_history: BTreeSet<(RostraId, RostraId, Timestamp, ShortEventId)>,
}

fn follow_strategy() -> impl Strategy<Value = (Vec<FollowSpec>, Plan, Plan)> {
    (
        prop::collection::vec(
            (0u8..2, 0u8..2, 0u8..4, 0u8..4).prop_map(|(author, followee, timestamp, selector)| {
                FollowSpec {
                    author,
                    followee,
                    timestamp,
                    selector,
                }
            }),
            1..=7,
        ),
        plan_strategy(),
        plan_strategy(),
    )
}

fn follow_inputs(specs: &[FollowSpec]) -> Vec<FollowInput> {
    let authors = [
        RostraIdSecretKey::from_bytes([31; 32]),
        RostraIdSecretKey::from_bytes([32; 32]),
    ];
    let followees = [
        RostraIdSecretKey::from_bytes([33; 32]).id(),
        RostraIdSecretKey::from_bytes([34; 32]).id(),
    ];
    specs
        .iter()
        .map(|spec| {
            let secret = authors[usize::from(spec.author) % authors.len()];
            let followee = followees[usize::from(spec.followee) % followees.len()];
            let selector = match spec.selector {
                0 => None,
                1 => Some(PersonaSelector::Only {
                    ids: vec![PersonaId(0)],
                }),
                2 => Some(PersonaSelector::Except {
                    ids: vec![PersonaId(1)],
                }),
                _ => Some(PersonaSelector::Except { ids: vec![] }),
            };
            let expected_selector = match spec.selector {
                0 => None,
                1 => Some(PersonasTagsSelector::Only {
                    ids: BTreeSet::from([PersonaTag::personal()]),
                }),
                2 => Some(PersonasTagsSelector::Except {
                    ids: BTreeSet::from([PersonaTag::professional()]),
                }),
                _ => Some(PersonasTagsSelector::Except {
                    ids: BTreeSet::new(),
                }),
            };
            let content = Follow {
                followee,
                persona: None,
                selector,
                persona_tags_selector: None,
            };
            let event = signed_content(secret, &content, 30_000 + u64::from(spec.timestamp));
            FollowInput {
                event,
                author: secret.id(),
                followee,
                selector: expected_selector,
            }
        })
        .collect()
}

fn follow_model(inputs: &[FollowInput]) -> FollowSnapshot {
    let mut winning: BTreeMap<(RostraId, RostraId), &FollowInput> = BTreeMap::new();
    for input in inputs {
        winning
            .entry((input.author, input.followee))
            .and_modify(|current| {
                if order(&current.event) < order(&input.event) {
                    *current = input;
                }
            })
            .or_insert(input);
    }
    let mut active = BTreeMap::new();
    let mut reverse = BTreeSet::new();
    let mut winners = BTreeMap::new();
    let mut unfollowed = BTreeMap::new();
    let mut follow_history = BTreeSet::new();
    for (&key, group) in &inputs.iter().fold(
        BTreeMap::<(RostraId, RostraId), Vec<&FollowInput>>::new(),
        |mut groups, input| {
            groups
                .entry((input.author, input.followee))
                .or_default()
                .push(input);
            groups
        },
    ) {
        let boundary = group
            .iter()
            .filter(|input| input.selector.is_none())
            .max_by_key(|input| order(&input.event))
            .copied();
        if let Some(boundary) = boundary {
            unfollowed.insert(key, order(&boundary.event));
        }
        for input in group.iter().copied().filter(|input| {
            input.selector.is_some()
                && boundary.is_none_or(|boundary| order(&boundary.event) < order(&input.event))
        }) {
            follow_history.insert((
                input.author,
                input.followee,
                input.event.timestamp(),
                input.event.event_id().to_short(),
            ));
        }
    }
    for ((author, followee), winner) in winning {
        winners.insert(
            (
                author,
                EventAuxKey::from_bytes(followee.to_short().to_bytes()).to_bytes(),
            ),
            order(&winner.event),
        );
        if let Some(selector) = &winner.selector {
            let first_ts = follow_history
                .iter()
                .filter(|(candidate_author, candidate_followee, _, _)| {
                    *candidate_author == author && *candidate_followee == followee
                })
                .map(|(_, _, timestamp, _)| *timestamp)
                .min()
                .expect("active follow has epoch history");
            active.insert(
                (author, followee),
                ActiveFollow {
                    latest_ts: winner.event.timestamp(),
                    latest_event_id: winner.event.event_id().to_short(),
                    first_ts,
                    selector: selector.clone(),
                },
            );
            reverse.insert((followee, author));
        }
    }
    FollowSnapshot {
        active,
        reverse,
        winners,
        unfollowed,
        follow_history,
    }
}

async fn follow_snapshot(db: &Database) -> DbResult<FollowSnapshot> {
    db.read_with(|tx| {
        let active = tx
            .open_table(&ids_followees::TABLE)?
            .range(..)?
            .map(|entry| {
                entry.map(|(key, value)| {
                    let value = value.value();
                    (
                        key.value(),
                        ActiveFollow {
                            latest_ts: value.latest_ts,
                            latest_event_id: value.latest_event_id,
                            first_ts: value.first_ts,
                            selector: value.effective_tags_selector(),
                        },
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        let reverse = tx
            .open_table(&ids_followers::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<_, _>>()?;
        let winners = tx
            .open_table(&events_singletons_new::TABLE)?
            .range(..)?
            .filter_map(|entry| match entry {
                Ok((key, value)) if key.value().1 == EventKind::FOLLOW => Some(Ok((
                    (key.value().0, key.value().2.to_bytes()),
                    (value.value().ts, value.value().inner.event_id),
                ))),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_, _>>()?;
        let unfollowed = tx
            .open_table(&ids_unfollowed::TABLE)?
            .range(..)?
            .map(|entry| {
                entry.map(|(key, value)| {
                    let value = value.value();
                    (key.value(), (value.ts, value.event_id))
                })
            })
            .collect::<Result<_, _>>()?;
        let follow_history = tx
            .open_table(&ids_follow_events::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<_, _>>()?;
        Ok(FollowSnapshot {
            active,
            reverse,
            winners,
            unfollowed,
            follow_history,
        })
    })
    .await
}

async fn check_follow(input: (Vec<FollowSpec>, Plan, Plan)) -> Result<(), String> {
    let (specs, first_plan, second_plan) = input;
    let inputs = follow_inputs(&specs);
    let expected = follow_model(&inputs);
    let events: Vec<_> = inputs.iter().map(|input| input.event.clone()).collect();
    let self_id = RostraIdSecretKey::from_bytes([39; 32]).id();
    let replicas = run_pair(self_id, &events, &first_plan, &second_plan)
        .await
        .map_err(|error| error.to_string())?;
    let first = follow_snapshot(&replicas.first)
        .await
        .map_err(|error| error.to_string())?;
    let second = follow_snapshot(&replicas.second)
        .await
        .map_err(|error| error.to_string())?;
    if first != expected || second != expected || first != second {
        return Err(format!(
            "follow mismatch\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
        ));
    }
    Ok(())
}

/// Equal-time and strict-time follow reducers converge by total event order.
#[test]
fn prop_follow_semantics_converge() {
    run_property(
        concat!(module_path!(), "::prop_follow_semantics_converge"),
        12,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/follow.txt"
        ),
        follow_strategy(),
        check_follow,
    );
}

#[derive(Clone, Debug)]
struct LatestSpec {
    author: u8,
    timestamp: u8,
    generic: bool,
    key: u8,
    value: u8,
}

struct LatestInput {
    event: VerifiedEventContent,
    author: RostraId,
    key: LatestKey,
    value: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LatestKey {
    Profile,
    Generic([u8; 16]),
}

#[derive(Debug, PartialEq, Eq)]
struct ProfileValue {
    timestamp: Timestamp,
    event_id: ShortEventId,
    display_name: String,
    bio: String,
    avatar: Option<(String, Vec<u8>)>,
}

#[derive(Debug, PartialEq, Eq)]
struct LatestSnapshot {
    profiles: BTreeMap<RostraId, ProfileValue>,
    singletons: BTreeMap<(RostraId, EventKind, [u8; 16]), (Timestamp, ShortEventId)>,
}

fn latest_strategy() -> impl Strategy<Value = (Vec<LatestSpec>, Plan, Plan)> {
    (
        prop::collection::vec(
            (0u8..2, 0u8..4, any::<bool>(), 0u8..2, any::<u8>()).prop_map(
                |(author, timestamp, generic, key, value)| LatestSpec {
                    author,
                    timestamp,
                    generic,
                    key,
                    value,
                },
            ),
            1..=7,
        ),
        plan_strategy(),
        plan_strategy(),
    )
}

fn latest_inputs(specs: &[LatestSpec]) -> Vec<LatestInput> {
    let authors = [
        RostraIdSecretKey::from_bytes([41; 32]),
        RostraIdSecretKey::from_bytes([42; 32]),
    ];
    specs
        .iter()
        .map(|spec| {
            let secret = authors[usize::from(spec.author) % authors.len()];
            if spec.generic {
                let key = EventAuxKey::from_bytes([spec.key + 1; 16]);
                let raw = EventContentRaw::new(vec![spec.value]);
                let event = Event::builder_raw_content()
                    .author(secret.id())
                    .kind(EventKind::RAW)
                    .singleton_aux_key(key)
                    .content(&raw)
                    .timestamp(
                        time::OffsetDateTime::from_unix_timestamp(
                            40_000 + i64::from(spec.timestamp),
                        )
                        .expect("property timestamp"),
                    )
                    .build();
                let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
                    .expect("deterministic event verifies");
                LatestInput {
                    event: with_content(event, raw),
                    author: secret.id(),
                    key: LatestKey::Generic(key.to_bytes()),
                    value: spec.value,
                }
            } else {
                let profile = SocialProfileUpdate {
                    display_name: format!("name-{}", spec.value),
                    bio: format!("bio-{}", spec.value),
                    avatar: (spec.value % 2 == 1)
                        .then(|| ("image/test".to_owned(), vec![spec.value])),
                };
                LatestInput {
                    event: signed_content(secret, &profile, 40_000 + u64::from(spec.timestamp)),
                    author: secret.id(),
                    key: LatestKey::Profile,
                    value: spec.value,
                }
            }
        })
        .collect()
}

fn latest_model(inputs: &[LatestInput]) -> LatestSnapshot {
    let mut winning: BTreeMap<(RostraId, LatestKey), &LatestInput> = BTreeMap::new();
    for input in inputs {
        winning
            .entry((input.author, input.key))
            .and_modify(|current| {
                if order(&current.event) < order(&input.event) {
                    *current = input;
                }
            })
            .or_insert(input);
    }
    let mut profiles = BTreeMap::new();
    let mut singletons = BTreeMap::new();
    for ((author, key), input) in winning {
        singletons.insert(
            (author, input.event.kind(), input.event.aux_key().to_bytes()),
            order(&input.event),
        );
        match key {
            LatestKey::Profile => {
                profiles.insert(
                    author,
                    ProfileValue {
                        timestamp: input.event.timestamp(),
                        event_id: input.event.event_id().to_short(),
                        display_name: format!("name-{}", input.value),
                        bio: format!("bio-{}", input.value),
                        avatar: (input.value % 2 == 1)
                            .then(|| ("image/test".to_owned(), vec![input.value])),
                    },
                );
            }
            LatestKey::Generic(_) => {}
        }
    }
    LatestSnapshot {
        profiles,
        singletons,
    }
}

async fn latest_snapshot(db: &Database) -> DbResult<LatestSnapshot> {
    db.read_with(|tx| {
        let profiles = tx
            .open_table(&social_profiles::TABLE)?
            .range(..)?
            .map(|entry| {
                entry.map(|(key, value)| {
                    let value = value.value();
                    (
                        key.value(),
                        ProfileValue {
                            timestamp: value.ts,
                            event_id: value.inner.event_id,
                            display_name: value.inner.display_name,
                            bio: value.inner.bio,
                            avatar: value.inner.avatar,
                        },
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        let singletons = tx
            .open_table(&events_singletons_new::TABLE)?
            .range(..)?
            .filter_map(|entry| match entry {
                Ok((key, value))
                    if matches!(
                        key.value().1,
                        EventKind::RAW | EventKind::SOCIAL_PROFILE_UPDATE
                    ) =>
                {
                    Some(Ok((
                        (key.value().0, key.value().1, key.value().2.to_bytes()),
                        (value.value().ts, value.value().inner.event_id),
                    )))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_, _>>()?;
        Ok(LatestSnapshot {
            profiles,
            singletons,
        })
    })
    .await
}

async fn check_latest(input: (Vec<LatestSpec>, Plan, Plan)) -> Result<(), String> {
    let (specs, first_plan, second_plan) = input;
    let inputs = latest_inputs(&specs);
    let expected = latest_model(&inputs);
    let events: Vec<_> = inputs.iter().map(|input| input.event.clone()).collect();
    let self_id = RostraIdSecretKey::from_bytes([49; 32]).id();
    let replicas = run_pair(self_id, &events, &first_plan, &second_plan)
        .await
        .map_err(|error| error.to_string())?;
    let first = latest_snapshot(&replicas.first)
        .await
        .map_err(|error| error.to_string())?;
    let second = latest_snapshot(&replicas.second)
        .await
        .map_err(|error| error.to_string())?;
    if first != expected || second != expected || first != second {
        return Err(format!(
            "latest-value mismatch\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
        ));
    }
    Ok(())
}

/// Equal-time and strict-time profile and generic singleton reducers converge.
#[test]
fn prop_profile_and_singleton_semantics_converge() {
    run_property(
        concat!(
            module_path!(),
            "::prop_profile_and_singleton_semantics_converge"
        ),
        12,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/latest.txt"
        ),
        latest_strategy(),
        check_latest,
    );
}

#[derive(Clone, Debug)]
struct VoteSpec {
    voter: u8,
    post: u8,
    timestamp: u8,
    value: u8,
}

struct VoteInput {
    event: VerifiedEventContent,
    voter: RostraId,
    post: ExternalEventId,
    value: Option<bool>,
}

type VoteKey = (RostraId, ExternalEventId);
type ProjectedVote = Option<(ShortEventId, Option<bool>)>;

#[derive(Debug, PartialEq, Eq)]
struct VoteSnapshot {
    votes: BTreeMap<VoteKey, ProjectedVote>,
    winners: BTreeMap<(RostraId, [u8; 16]), (Timestamp, ShortEventId)>,
    sums: BTreeMap<ExternalEventId, i64>,
}

fn vote_strategy() -> impl Strategy<Value = (Vec<VoteSpec>, Plan, Plan)> {
    (
        prop::collection::vec(
            (0u8..3, 0u8..4, 0u8..4, 0u8..3).prop_map(|(voter, post, timestamp, value)| VoteSpec {
                voter,
                post,
                timestamp,
                value,
            }),
            1..=8,
        ),
        plan_strategy(),
        plan_strategy(),
    )
}

fn vote_inputs(specs: &[VoteSpec]) -> Vec<VoteInput> {
    let voters = [
        RostraIdSecretKey::from_bytes([51; 32]),
        RostraIdSecretKey::from_bytes([52; 32]),
        RostraIdSecretKey::from_bytes([53; 32]),
    ];
    let post_authors = [
        RostraIdSecretKey::from_bytes([54; 32]).id(),
        RostraIdSecretKey::from_bytes([55; 32]).id(),
    ];
    let posts = [
        ExternalEventId::new(post_authors[0], ShortEventId::from_bytes([1; 16])),
        ExternalEventId::new(post_authors[1], ShortEventId::from_bytes([1; 16])),
        ExternalEventId::new(post_authors[0], ShortEventId::from_bytes([2; 16])),
        ExternalEventId::new(post_authors[1], ShortEventId::from_bytes([2; 16])),
    ];
    specs
        .iter()
        .map(|spec| {
            let secret = voters[usize::from(spec.voter) % voters.len()];
            let post = posts[usize::from(spec.post) % posts.len()];
            let value = match spec.value {
                0 => None,
                1 => Some(false),
                _ => Some(true),
            };
            VoteInput {
                event: signed_content(
                    secret,
                    &SocialVote::new(post, value),
                    50_000 + u64::from(spec.timestamp),
                ),
                voter: secret.id(),
                post,
                value,
            }
        })
        .collect()
}

fn vote_model(inputs: &[VoteInput]) -> VoteSnapshot {
    let mut winning: BTreeMap<(RostraId, [u8; 16]), &VoteInput> = BTreeMap::new();
    for input in inputs {
        winning
            .entry((
                input.voter,
                Database::social_vote_aux_key(input.post).to_bytes(),
            ))
            .and_modify(|current| {
                if order(&current.event) < order(&input.event) {
                    *current = input;
                }
            })
            .or_insert(input);
    }
    let mut votes = inputs
        .iter()
        .map(|input| ((input.voter, input.post), None))
        .collect::<BTreeMap<_, _>>();
    let mut winners = BTreeMap::new();
    let mut sums = BTreeMap::new();
    for ((voter, aux_key), input) in winning {
        votes.insert(
            (voter, input.post),
            Some((input.event.event_id().to_short(), input.value)),
        );
        winners.insert((voter, aux_key), order(&input.event));
        *sums.entry(input.post).or_default() += match input.value {
            Some(true) => 1,
            Some(false) => -1,
            None => 0,
        };
    }
    sums.retain(|_, sum| *sum != 0);
    VoteSnapshot {
        votes,
        winners,
        sums,
    }
}

async fn vote_snapshot(
    db: &Database,
    keys: &BTreeSet<(RostraId, ExternalEventId)>,
) -> DbResult<VoteSnapshot> {
    let winners = db
        .read_with(|tx| {
            tx.open_table(&events_singletons_new::TABLE)?
                .range(..)?
                .filter_map(|entry| match entry {
                    Ok((key, value)) if key.value().1 == EventKind::SOCIAL_VOTE => Some(Ok((
                        (key.value().0, key.value().2.to_bytes()),
                        (value.value().ts, value.value().inner.event_id),
                    ))),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map_err(Into::into)
        })
        .await?;
    let mut votes = BTreeMap::new();
    for &(voter, post) in keys {
        let aux = Database::social_vote_aux_key(post);
        let vote = db.get_social_vote(voter, post).await;
        let vote = vote.map(|vote| {
            let (_, winner) = winners
                .get(&(voter, aux.to_bytes()))
                .copied()
                .expect("projected vote has singleton winner");
            (winner, vote)
        });
        votes.insert((voter, post), vote);
    }
    let sums = db
        .read_with(|tx| {
            tx.open_table(&social_vote_sums::TABLE)?
                .range(..)?
                .filter_map(|entry| match entry {
                    Ok((key, value)) if value.value().current_sum != 0 => {
                        Some(Ok((key.value(), value.value().current_sum)))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<_, _>>()
                .map_err(Into::into)
        })
        .await?;
    Ok(VoteSnapshot {
        votes,
        winners,
        sums,
    })
}

async fn check_vote(input: (Vec<VoteSpec>, Plan, Plan)) -> Result<(), String> {
    let (specs, first_plan, second_plan) = input;
    let inputs = vote_inputs(&specs);
    let expected = vote_model(&inputs);
    let keys = expected.votes.keys().copied().collect();
    let events: Vec<_> = inputs.iter().map(|input| input.event.clone()).collect();
    let self_id = RostraIdSecretKey::from_bytes([59; 32]).id();
    let replicas = run_pair(self_id, &events, &first_plan, &second_plan)
        .await
        .map_err(|error| error.to_string())?;
    let first = vote_snapshot(&replicas.first, &keys)
        .await
        .map_err(|error| error.to_string())?;
    let second = vote_snapshot(&replicas.second, &keys)
        .await
        .map_err(|error| error.to_string())?;
    if first != expected || second != expected || first != second {
        return Err(format!(
            "vote mismatch\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
        ));
    }
    Ok(())
}

/// Equal-time and strict-time vote winners and numerical sums converge.
#[test]
fn prop_vote_semantics_converge() {
    run_property(
        concat!(module_path!(), "::prop_vote_semantics_converge"),
        12,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/vote.txt"
        ),
        vote_strategy(),
        check_vote,
    );
}
