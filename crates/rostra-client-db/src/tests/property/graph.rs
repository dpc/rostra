use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, SignedEvent, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};

use super::runner::{Plan, plan_strategy, run_pair, run_property, with_content};
use crate::{Database, DbResult, events, events_by_time, events_heads, events_missing, ids_full};

#[derive(Clone, Debug)]
struct EventSpec {
    author: u8,
    previous: u8,
    auxiliary: u8,
    delete_auxiliary: bool,
    timestamp: u8,
}

#[derive(Debug, PartialEq, Eq)]
struct GraphSnapshot {
    events: BTreeMap<ShortEventId, SignedEvent>,
    by_time: BTreeSet<(Timestamp, ShortEventId)>,
    heads: BTreeSet<(RostraId, ShortEventId)>,
    missing: BTreeMap<(RostraId, ShortEventId), Option<ShortEventId>>,
    authors: BTreeSet<RostraId>,
}

fn strategy() -> impl Strategy<Value = (Vec<EventSpec>, Plan, Plan)> {
    (
        prop::collection::vec(
            (0u8..3, any::<u8>(), any::<u8>(), any::<bool>(), 0u8..5).prop_map(
                |(author, previous, auxiliary, delete_auxiliary, timestamp)| EventSpec {
                    author,
                    previous,
                    auxiliary,
                    delete_auxiliary,
                    timestamp,
                },
            ),
            1..=8,
        ),
        plan_strategy(),
        plan_strategy(),
    )
}

fn parent(selector: u8, index: usize, events: &[VerifiedEventContent]) -> Option<ShortEventId> {
    match selector % 4 {
        0 => None,
        1 => Some(ShortEventId::from_bytes([0xd0 | (selector & 0x0f); 16])),
        _ if index == 0 => None,
        _ => Some(events[usize::from(selector) % index].event_id().to_short()),
    }
}

fn materialize(specs: &[EventSpec]) -> Vec<VerifiedEventContent> {
    let secrets = [
        RostraIdSecretKey::from_bytes([11; 32]),
        RostraIdSecretKey::from_bytes([12; 32]),
        RostraIdSecretKey::from_bytes([13; 32]),
    ];
    let mut events = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let secret = secrets[usize::from(spec.author) % secrets.len()];
        let content = EventContentRaw::new(vec![index as u8, spec.author, spec.timestamp]);
        let previous = parent(spec.previous, index, &events);
        let auxiliary = parent(spec.auxiliary, index, &events);
        let builder = Event::builder_raw_content()
            .author(secret.id())
            .kind(EventKind::RAW)
            .maybe_parent_prev(previous)
            .content(&content)
            .timestamp(
                time::OffsetDateTime::from_unix_timestamp(10_000 + i64::from(spec.timestamp))
                    .expect("property timestamp"),
            );
        let event = if spec.delete_auxiliary {
            match auxiliary {
                Some(auxiliary) => builder.delete(auxiliary).build(),
                None => builder.build(),
            }
        } else {
            builder.maybe_parent_aux(auxiliary).build()
        };
        let verified = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
            .expect("deterministic event verifies");
        events.push(with_content(verified, content));
    }
    events
}

fn model(materialized: &[VerifiedEventContent]) -> GraphSnapshot {
    let mut events = BTreeMap::new();
    let mut by_time = BTreeSet::new();
    let mut heads = BTreeSet::new();
    let mut missing_candidates: BTreeMap<(RostraId, ShortEventId), Vec<(Timestamp, ShortEventId)>> =
        BTreeMap::new();
    let mut authors = BTreeSet::new();

    for content in materialized {
        let event = content.event;
        let id = event.event_id.to_short();
        let author = event.author();
        events.insert(id, event.into());
        by_time.insert((event.timestamp(), id));
        heads.insert((author, id));
        authors.insert(author);
    }

    for content in materialized {
        let event = content.event;
        let author = event.author();
        let id = event.event_id.to_short();
        let parents = if event.parent_aux() == event.parent_prev() {
            vec![(event.parent_aux(), true)]
        } else {
            vec![(event.parent_aux(), true), (event.parent_prev(), false)]
        };
        for (parent, is_auxiliary) in parents {
            let Some(parent) = parent else {
                continue;
            };
            let resolves = materialized.iter().any(|candidate| {
                candidate.event_id().to_short() == parent && candidate.author() == author
            });
            if resolves {
                heads.remove(&(author, parent));
            } else {
                let candidates = missing_candidates.entry((author, parent)).or_default();
                if is_auxiliary && event.is_delete_parent_aux_content_set() {
                    candidates.push((event.timestamp(), id));
                }
            }
        }
    }
    let missing = missing_candidates
        .into_iter()
        .map(|(key, candidates)| {
            let deleted_by = candidates.into_iter().max().map(|(_, event_id)| event_id);
            (key, deleted_by)
        })
        .collect();

    GraphSnapshot {
        events,
        by_time,
        heads,
        missing,
        authors,
    }
}

async fn snapshot(db: &Database) -> DbResult<GraphSnapshot> {
    db.read_with(|tx| {
        let events = tx
            .open_table(&events::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, value)| (key.value(), value.value().signed)))
            .collect::<Result<_, _>>()?;
        let by_time = tx
            .open_table(&events_by_time::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<_, _>>()?;
        let heads = tx
            .open_table(&events_heads::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<_, _>>()?;
        let missing = tx
            .open_table(&events_missing::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, value)| (key.value(), value.value().deleted_by)))
            .collect::<Result<_, _>>()?;
        let authors = ids_full::read_all(tx)?.into_iter().collect();
        Ok(GraphSnapshot {
            events,
            by_time,
            heads,
            missing,
            authors,
        })
    })
    .await
}

async fn check(input: (Vec<EventSpec>, Plan, Plan)) -> Result<(), String> {
    let (specs, first_plan, second_plan) = input;
    let events = materialize(&specs);
    let expected = model(&events);
    let self_id = RostraIdSecretKey::from_bytes([19; 32]).id();
    let replicas = run_pair(self_id, &events, &first_plan, &second_plan)
        .await
        .map_err(|error| error.to_string())?;
    let first = snapshot(&replicas.first)
        .await
        .map_err(|error| error.to_string())?;
    let second = snapshot(&replicas.second)
        .await
        .map_err(|error| error.to_string())?;
    if first != expected || second != expected || first != second {
        return Err(format!(
            "graph mismatch\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
        ));
    }
    Ok(())
}

/// Author-relative graph indexes converge under independent durable schedules.
#[test]
fn prop_author_scoped_event_graph_converges() {
    run_property(
        concat!(module_path!(), "::prop_author_scoped_event_graph_converges"),
        20,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/graph.txt"
        ),
        strategy(),
        check,
    );
}
