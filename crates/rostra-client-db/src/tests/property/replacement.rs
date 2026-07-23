use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};

use super::runner::{
    Intervention, Plan, RollbackOracle, plan_strategy, run_pair_observed, run_property,
    with_content,
};
use super::usage::Usage;
use crate::event::EventContentState;
use crate::{
    Database, DbResult, content_rc, content_store, events_content_missing, events_content_state,
    ids_data_usage, social_news_rank_by_post_id, social_news_rank_by_score,
    social_news_rank_by_time, social_posts, social_posts_by_received_at, social_posts_by_time,
    social_posts_received_at_keys, social_posts_replaced_by, social_posts_replaces,
    social_posts_replies, social_posts_self_mention,
};

#[derive(Clone, Copy, Debug)]
enum DeletingBody {
    Absent,
    Empty,
    Whitespace,
    Edit,
}

impl DeletingBody {
    fn is_edit(self) -> bool {
        matches!(self, Self::Edit)
    }
}

const REPLACEMENT_CASES: [(DeletingBody, DeletingBody, bool); 8] = [
    (DeletingBody::Edit, DeletingBody::Edit, false),
    (DeletingBody::Edit, DeletingBody::Edit, true),
    (DeletingBody::Absent, DeletingBody::Edit, false),
    (DeletingBody::Empty, DeletingBody::Edit, false),
    (DeletingBody::Whitespace, DeletingBody::Edit, false),
    (DeletingBody::Edit, DeletingBody::Absent, false),
    (DeletingBody::Edit, DeletingBody::Empty, false),
    (DeletingBody::Edit, DeletingBody::Whitespace, false),
];

#[derive(Clone, Debug)]
struct ReplacementSpec {
    intermediate: DeletingBody,
    newest: DeletingBody,
    delete_newest: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SemanticState {
    Processed,
    Deleted(ShortEventId),
}

#[derive(Debug, PartialEq, Eq)]
struct ReplacementSnapshot {
    replaced_by: BTreeSet<(ShortEventId, ShortEventId)>,
    replaces: BTreeSet<(ShortEventId, ShortEventId)>,
    states: [SemanticState; 3],
    by_time: BTreeSet<ShortEventId>,
    replies: BTreeSet<ShortEventId>,
    reply_count: u64,
    news_index: BTreeSet<ShortEventId>,
    news_by_rank: BTreeSet<ShortEventId>,
    news_by_time: BTreeSet<ShortEventId>,
    mentions: BTreeSet<ShortEventId>,
    receipt_forward: BTreeSet<ShortEventId>,
    receipt_reverse: BTreeSet<ShortEventId>,
    visible: BTreeSet<ShortEventId>,
    resolved_original: Option<ShortEventId>,
}

struct Materialized {
    events: Vec<VerifiedEventContent>,
    author: RostraId,
    anchor: ShortEventId,
    chain: [ShortEventId; 3],
    final_deleter: Option<ShortEventId>,
}

fn strategy() -> impl Strategy<Value = (Plan, Plan)> {
    (plan_strategy(), plan_strategy())
}

fn social_post(
    secret: RostraIdSecretKey,
    timestamp: i64,
    previous: Option<rostra_core::EventId>,
    replaced: Option<rostra_core::EventId>,
    content: content_kind::SocialPost,
) -> VerifiedEventContent {
    let content = content
        .serialize_cbor()
        .expect("social post content serializes");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .content(&content)
        .maybe_parent_prev(previous.map(Into::into))
        .maybe_delete(replaced.map(Into::into))
        .build();
    let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
        .expect("deterministic social post verifies");
    with_content(event, content)
}

fn deleting_content(
    body: DeletingBody,
    reply_to: ExternalEventId,
    self_id: RostraId,
) -> content_kind::SocialPost {
    let post = match body {
        DeletingBody::Absent => {
            content_kind::SocialPost::new("👍".to_owned(), Some(reply_to), Default::default())
        }
        DeletingBody::Empty => {
            content_kind::SocialPost::new_text(String::new(), Some(reply_to), Default::default())
        }
        DeletingBody::Whitespace => content_kind::SocialPost::new_text(
            " \n\t".to_owned(),
            Some(reply_to),
            Default::default(),
        ),
        DeletingBody::Edit => content_kind::SocialPost::new_text(
            format!("edit; hello <rostra:{self_id}>"),
            Some(reply_to),
            Default::default(),
        ),
    };
    post.with_news_fields(None, Some("property news".to_owned()))
}

fn materialize(spec: &ReplacementSpec) -> Materialized {
    let secret = RostraIdSecretKey::from_bytes([51; 32]);
    let author = secret.id();
    let self_id = RostraIdSecretKey::from_bytes([59; 32]).id();
    let anchor = social_post(
        secret,
        40_000,
        None,
        None,
        content_kind::SocialPost::new_text("anchor".to_owned(), None, Default::default()),
    );
    let original = social_post(
        secret,
        40_001,
        Some(anchor.event_id()),
        None,
        content_kind::SocialPost::new_text("original".to_owned(), None, Default::default()),
    );
    let reply_to = ExternalEventId::new(author, anchor.event_id());
    let intermediate = social_post(
        secret,
        40_002,
        Some(original.event_id()),
        Some(original.event_id()),
        deleting_content(spec.intermediate, reply_to, self_id),
    );
    let newest = social_post(
        secret,
        40_003,
        Some(intermediate.event_id()),
        Some(intermediate.event_id()),
        deleting_content(spec.newest, reply_to, self_id),
    );
    let chain = [
        original.event_id().to_short(),
        intermediate.event_id().to_short(),
        newest.event_id().to_short(),
    ];
    let mut events = vec![anchor.clone(), original, intermediate, newest];
    let final_deleter = spec.delete_newest.then(|| {
        let content = EventContentRaw::new(vec![]);
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::RAW)
            .timestamp(time::OffsetDateTime::from_unix_timestamp(40_004).expect("valid timestamp"))
            .parent_prev(events[3].event_id().into())
            .delete(events[3].event_id().into())
            .content(&content)
            .build();
        let event = VerifiedEvent::verify_signed(author, event.signed_by(secret))
            .expect("deterministic final deletion verifies");
        let id = event.event_id.to_short();
        events.push(with_content(event, content));
        id
    });
    Materialized {
        events,
        author,
        anchor: anchor.event_id().to_short(),
        chain,
        final_deleter,
    }
}

fn model(spec: &ReplacementSpec, materialized: &Materialized) -> ReplacementSnapshot {
    let [original, intermediate, newest] = materialized.chain;
    let mut replaced_by = BTreeSet::new();
    if spec.intermediate.is_edit() {
        replaced_by.insert((original, intermediate));
    }
    if spec.newest.is_edit() {
        replaced_by.insert((intermediate, newest));
    }
    let replaces = replaced_by.iter().map(|(old, new)| (*new, *old)).collect();
    let newest_visible = spec.newest.is_edit() && materialized.final_deleter.is_none();
    let projected: BTreeSet<_> = newest_visible.then_some(newest).into_iter().collect();
    let visible: BTreeSet<_> = std::iter::once(materialized.anchor)
        .chain(newest_visible.then_some(newest))
        .collect();
    let receipt_members = visible.clone();
    ReplacementSnapshot {
        replaced_by,
        replaces,
        states: [
            SemanticState::Deleted(intermediate),
            SemanticState::Deleted(newest),
            materialized
                .final_deleter
                .map_or(SemanticState::Processed, SemanticState::Deleted),
        ],
        by_time: visible.clone(),
        replies: projected.clone(),
        reply_count: u64::from(newest_visible),
        news_index: projected.clone(),
        news_by_rank: projected.clone(),
        news_by_time: projected.clone(),
        mentions: projected,
        receipt_forward: receipt_members.clone(),
        receipt_reverse: receipt_members,
        visible,
        resolved_original: (spec.intermediate.is_edit() && spec.newest.is_edit() && newest_visible)
            .then_some(newest),
    }
}

async fn snapshot(db: &Database, materialized: &Materialized) -> DbResult<ReplacementSnapshot> {
    let [original, intermediate, newest] = materialized.chain;
    let author = materialized.author;
    let anchor = materialized.anchor;
    let tables = db
        .read_with(|tx| {
            let replaced_by = tx
                .open_table(&social_posts_replaced_by::TABLE)?
                .range(
                    &(author, ShortEventId::ZERO, ShortEventId::ZERO)
                        ..=&(author, ShortEventId::MAX, ShortEventId::MAX),
                )?
                .map(|entry| {
                    entry.map(|(key, _)| {
                        let (_, old, new) = key.value();
                        (old, new)
                    })
                })
                .collect::<Result<_, _>>()?;
            let replaces = tx
                .open_table(&social_posts_replaces::TABLE)?
                .range(
                    &(author, ShortEventId::ZERO, ShortEventId::ZERO)
                        ..=&(author, ShortEventId::MAX, ShortEventId::MAX),
                )?
                .map(|entry| {
                    entry.map(|(key, _)| {
                        let (_, new, old) = key.value();
                        (new, old)
                    })
                })
                .collect::<Result<_, _>>()?;
            let states_table = tx.open_table(&events_content_state::TABLE)?;
            let states = [original, intermediate, newest].map(|event_id| {
                match states_table
                    .get(&event_id)
                    .expect("state read succeeds")
                    .map(|entry| entry.value())
                {
                    None => SemanticState::Processed,
                    Some(EventContentState::Deleted { deleted_by }) => {
                        SemanticState::Deleted(deleted_by)
                    }
                    state => panic!("replacement fence left state {state:?}"),
                }
            });
            let by_time = tx
                .open_table(&social_posts_by_time::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value().1))
                .collect::<Result<_, _>>()?;
            let replies = tx
                .open_table(&social_posts_replies::TABLE)?
                .range(
                    &(anchor, rostra_core::Timestamp::ZERO, ShortEventId::ZERO)
                        ..=&(anchor, rostra_core::Timestamp::MAX, ShortEventId::MAX),
                )?
                .map(|entry| entry.map(|(key, _)| key.value().2))
                .collect::<Result<_, _>>()?;
            let reply_count = tx
                .open_table(&social_posts::TABLE)?
                .get(&anchor)?
                .map(|entry| entry.value().reply_count)
                .unwrap_or_default();
            let news = tx
                .open_table(&social_news_rank_by_post_id::TABLE)?
                .range(..)?
                .filter_map(|entry| match entry {
                    Ok((key, _)) if key.value().rostra_id() == author => {
                        Some(Ok(key.value().event_id()))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<_, _>>()?;
            let mentions = tx
                .open_table(&social_posts_self_mention::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value()))
                .collect::<Result<_, _>>()?;
            let receipt_forward = tx
                .open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(_, event_id)| event_id.value()))
                .collect::<Result<_, _>>()?;
            let receipt_reverse = tx
                .open_table(&social_posts_received_at_keys::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(event_id, _)| event_id.value()))
                .collect::<Result<_, _>>()?;
            Ok((
                replaced_by,
                replaces,
                states,
                by_time,
                replies,
                reply_count,
                news,
                mentions,
                receipt_forward,
                receipt_reverse,
            ))
        })
        .await?;
    let (visible, _) = db.paginate_social_posts_rev(None, 10, |_| true).await;
    let (news_by_rank, _) = db.paginate_news_posts_by_rank_rev(None, 10).await;
    let (news_by_time, _) = db.paginate_news_posts_by_time_rev(None, 10).await;
    Ok(ReplacementSnapshot {
        replaced_by: tables.0,
        replaces: tables.1,
        states: tables.2,
        by_time: tables.3,
        replies: tables.4,
        reply_count: tables.5,
        news_index: tables.6,
        news_by_rank: news_by_rank
            .into_iter()
            .map(|post| post.post_id.event_id())
            .collect(),
        news_by_time: news_by_time
            .into_iter()
            .map(|post| post.post_id.event_id())
            .collect(),
        mentions: tables.7,
        receipt_forward: tables.8,
        receipt_reverse: tables.9,
        visible: visible.into_iter().map(|post| post.event_id).collect(),
        resolved_original: db.get_social_post(original).await.map(|post| post.event_id),
    })
}

struct NoReplacementIntervention;

impl Intervention for NoReplacementIntervention {
    fn apply(
        &self,
        _db: &Database,
        _events: &[VerifiedEventContent],
        _tx: &crate::WriteTransactionCtx,
    ) -> DbResult<()> {
        Ok(())
    }
}

struct ReplacementRollbackOracle;

#[derive(Debug, PartialEq, Eq)]
struct ReplacementRollbackSnapshot {
    states: BTreeMap<ShortEventId, EventContentState>,
    stored: BTreeSet<rostra_core::ContentHash>,
    reference_counts: BTreeSet<(rostra_core::ContentHash, u64)>,
    fetch_queue: BTreeSet<(rostra_core::Timestamp, ShortEventId)>,
    usage: BTreeMap<RostraId, Usage>,
    replaced_by: BTreeSet<(RostraId, ShortEventId, ShortEventId)>,
    replaces: BTreeSet<(RostraId, ShortEventId, ShortEventId)>,
    by_time: BTreeSet<(rostra_core::Timestamp, ShortEventId)>,
    replies: BTreeSet<(ShortEventId, rostra_core::Timestamp, ShortEventId)>,
    reply_counts: BTreeSet<(ShortEventId, u64, u64)>,
    news_by_post: BTreeSet<ExternalEventId>,
    news_by_rank: BTreeSet<(crate::SocialVoteScore, ExternalEventId)>,
    news_by_time: BTreeSet<(rostra_core::Timestamp, ExternalEventId)>,
    mentions: BTreeSet<ShortEventId>,
    receipt_forward: BTreeSet<((rostra_core::Timestamp, u64), ShortEventId)>,
    receipt_reverse: BTreeSet<(ShortEventId, (rostra_core::Timestamp, u64))>,
}

impl RollbackOracle for ReplacementRollbackOracle {
    type Snapshot = ReplacementRollbackSnapshot;

    async fn snapshot(
        &self,
        db: &Database,
        _events: &[VerifiedEventContent],
    ) -> DbResult<Self::Snapshot> {
        db.read_with(|tx| {
            Ok(ReplacementRollbackSnapshot {
                states: tx
                    .open_table(&events_content_state::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, value)| (key.value(), value.value())))
                    .collect::<Result<_, _>>()?,
                stored: tx
                    .open_table(&content_store::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                reference_counts: tx
                    .open_table(&content_rc::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, value)| (key.value(), value.value())))
                    .collect::<Result<_, _>>()?,
                fetch_queue: tx
                    .open_table(&events_content_missing::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                usage: tx
                    .open_table(&ids_data_usage::TABLE)?
                    .range(..)?
                    .map(|entry| {
                        entry.map(|(key, value)| (key.value(), Usage::from(value.value())))
                    })
                    .collect::<Result<_, _>>()?,
                replaced_by: tx
                    .open_table(&social_posts_replaced_by::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                replaces: tx
                    .open_table(&social_posts_replaces::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                by_time: tx
                    .open_table(&social_posts_by_time::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                replies: tx
                    .open_table(&social_posts_replies::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                reply_counts: tx
                    .open_table(&social_posts::TABLE)?
                    .range(..)?
                    .map(|entry| {
                        entry.map(|(key, value)| {
                            let value = value.value();
                            (key.value(), value.reply_count, value.reaction_count)
                        })
                    })
                    .collect::<Result<_, _>>()?,
                news_by_post: tx
                    .open_table(&social_news_rank_by_post_id::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                news_by_rank: tx
                    .open_table(&social_news_rank_by_score::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                news_by_time: tx
                    .open_table(&social_news_rank_by_time::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                mentions: tx
                    .open_table(&social_posts_self_mention::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, _)| key.value()))
                    .collect::<Result<_, _>>()?,
                receipt_forward: tx
                    .open_table(&social_posts_by_received_at::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, value)| (key.value(), value.value())))
                    .collect::<Result<_, _>>()?,
                receipt_reverse: tx
                    .open_table(&social_posts_received_at_keys::TABLE)?
                    .range(..)?
                    .map(|entry| entry.map(|(key, value)| (key.value(), value.value())))
                    .collect::<Result<_, _>>()?,
            })
        })
        .await
    }
}

async fn check(input: (Plan, Plan)) -> Result<(), String> {
    let (first_plan, second_plan) = input;
    for (case_index, (intermediate, newest, delete_newest)) in
        REPLACEMENT_CASES.into_iter().enumerate()
    {
        let spec = ReplacementSpec {
            intermediate,
            newest,
            delete_newest,
        };
        let materialized = materialize(&spec);
        let expected = model(&spec, &materialized);
        let self_id = RostraIdSecretKey::from_bytes([59; 32]).id();
        let (case_first_plan, case_second_plan) = if case_index < 2 {
            (first_plan.clone(), second_plan.clone())
        } else {
            // The live and finally deleted edit chains exercise reopen. The six
            // additional blank-shape positions retain every other schedule
            // dimension without multiplying disk-open cost.
            (first_plan.without_reopens(), second_plan.without_reopens())
        };
        let replicas = run_pair_observed(
            self_id,
            &materialized.events,
            &[] as &[NoReplacementIntervention],
            &ReplacementRollbackOracle,
            &case_first_plan,
            &case_second_plan,
        )
        .await
        .map_err(|error| error.to_string())?;
        let first = snapshot(&replicas.first, &materialized)
            .await
            .map_err(|error| error.to_string())?;
        let second = snapshot(&replicas.second, &materialized)
            .await
            .map_err(|error| error.to_string())?;
        if first != expected || second != expected || first != second {
            return Err(format!(
                "replacement mismatch\nspec={spec:#?}\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
            ));
        }
    }
    Ok(())
}

/// Replacement lineage composes with social projection reversion.
#[test]
fn prop_replacement_projection_reversion_converges() {
    run_property(
        concat!(
            module_path!(),
            "::prop_replacement_projection_reversion_converges"
        ),
        1,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/replacement.txt"
        ),
        strategy(),
        check,
    );
}
