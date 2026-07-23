use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ContentHash, ShortEventId};

use super::runner::{
    Intervention, Plan, RollbackOracle, intervention_plan_strategy, run_pair_observed,
    run_property, with_content,
};
use super::usage::Usage;
use crate::event::EventContentState;
use crate::{
    Database, DbResult, WriteTransactionCtx, content_rc, content_store, events,
    events_content_missing, events_content_state, ids_data_usage,
};

#[derive(Clone, Debug)]
struct TargetSpec {
    payload: u8,
    byte: u8,
    deleted: bool,
    pruned: bool,
}

#[derive(Clone, Copy, Debug)]
enum LifecycleIntervention {
    Prune(usize),
    Collect(usize),
}

impl Intervention for LifecycleIntervention {
    fn sort_key(&self, _index: usize, _event_count: usize) -> (usize, u8) {
        match *self {
            Self::Prune(index) => (index, 1),
            Self::Collect(index) => (index, 3),
        }
    }

    fn apply(
        &self,
        _db: &Database,
        materialized: &[VerifiedEventContent],
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        let index = match *self {
            Self::Prune(index) | Self::Collect(index) => index,
        };
        let event = &materialized[index];
        if tx
            .open_table(&events::TABLE)?
            .get(&event.event_id().to_short())?
            .is_none()
        {
            return Ok(());
        }

        match *self {
            Self::Prune(_) => {
                let mut states = tx.open_table(&events_content_state::TABLE)?;
                let mut reference_counts = tx.open_table(&content_rc::TABLE)?;
                let mut queue = tx.open_table(&events_content_missing::TABLE)?;
                let mut usage = tx.open_table(&ids_data_usage::TABLE)?;
                Database::prune_event_content_tx(
                    event.event_id(),
                    event.content_hash(),
                    &mut states,
                    &mut reference_counts,
                    &mut queue,
                    Some((event.author(), event.content_len(), &mut usage)),
                )?;
            }
            Self::Collect(_) => {
                let reference_counts = tx.open_table(&content_rc::TABLE)?;
                if Database::get_content_rc_tx(event.content_hash(), &reference_counts)? == 0 {
                    tx.open_table(&content_store::TABLE)?
                        .remove(&event.content_hash())?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SemanticState {
    Processed,
    Deleted(ShortEventId),
    Pruned,
}

#[derive(Debug, PartialEq, Eq)]
struct LifecycleSnapshot {
    states: BTreeMap<ShortEventId, SemanticState>,
    available: BTreeSet<ShortEventId>,
    reference_counts: BTreeMap<ContentHash, u64>,
    fetch_queue: BTreeSet<ShortEventId>,
    usage: BTreeMap<RostraId, Usage>,
}

#[derive(Debug, PartialEq, Eq)]
struct LifecycleRollbackSnapshot {
    states: BTreeMap<ShortEventId, EventContentState>,
    stored: BTreeSet<ContentHash>,
    reference_counts: BTreeMap<ContentHash, u64>,
    fetch_queue: BTreeSet<(rostra_core::Timestamp, ShortEventId)>,
    usage: BTreeMap<RostraId, Usage>,
}

struct LifecycleRollbackOracle;

impl RollbackOracle for LifecycleRollbackOracle {
    type Snapshot = LifecycleRollbackSnapshot;

    async fn snapshot(
        &self,
        db: &Database,
        _events: &[VerifiedEventContent],
    ) -> DbResult<Self::Snapshot> {
        db.read_with(|tx| {
            Ok(LifecycleRollbackSnapshot {
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
            })
        })
        .await
    }
}

struct Materialized {
    events: Vec<VerifiedEventContent>,
    target_count: usize,
    deleters: BTreeMap<usize, ShortEventId>,
}

fn strategy() -> impl Strategy<Value = (Vec<TargetSpec>, Plan, Plan)> {
    (
        prop::collection::vec(
            (0u8..4, any::<u8>(), any::<bool>(), any::<bool>()).prop_map(
                |(payload, byte, deleted, pruned)| TargetSpec {
                    payload,
                    byte,
                    deleted,
                    pruned,
                },
            ),
            1..=4,
        ),
        intervention_plan_strategy(),
        intervention_plan_strategy(),
    )
}

fn payload(spec: &TargetSpec) -> EventContentRaw {
    EventContentRaw::new(match spec.payload {
        0 => vec![0x41],
        1 => vec![0x42, 0x42],
        2 => vec![spec.byte; usize::from(spec.byte % 4) + 1],
        _ => vec![spec.byte, 0x43, spec.byte],
    })
}

fn materialize(specs: &[TargetSpec]) -> Materialized {
    let secret = RostraIdSecretKey::from_bytes([31; 32]);
    let mut events = specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let content = payload(spec);
            let event = Event::builder_raw_content()
                .author(secret.id())
                .kind(EventKind::RAW)
                .content(&content)
                .timestamp(
                    time::OffsetDateTime::from_unix_timestamp(30_000 + index as i64)
                        .expect("property timestamp"),
                )
                .build();
            let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
                .expect("deterministic target verifies");
            with_content(event, content)
        })
        .collect::<Vec<_>>();
    let target_count = events.len();
    let mut deleters = BTreeMap::new();
    for (index, spec) in specs.iter().enumerate() {
        if !spec.deleted {
            continue;
        }
        let content = EventContentRaw::new(vec![]);
        let target = events[index].event_id();
        let event = Event::builder_raw_content()
            .author(secret.id())
            .kind(EventKind::RAW)
            .parent_prev(target.into())
            .delete(target.into())
            .content(&content)
            .timestamp(
                time::OffsetDateTime::from_unix_timestamp(31_000 + index as i64)
                    .expect("property timestamp"),
            )
            .build();
        let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
            .expect("deterministic deletion verifies");
        deleters.insert(index, event.event_id.to_short());
        events.push(with_content(event, content));
    }
    Materialized {
        events,
        target_count,
        deleters,
    }
}

fn interventions(specs: &[TargetSpec]) -> Vec<LifecycleIntervention> {
    let mut interventions = specs
        .iter()
        .enumerate()
        .filter(|(_, spec)| spec.pruned)
        .map(|(index, _)| LifecycleIntervention::Prune(index))
        .collect::<Vec<_>>();
    interventions.extend((0..specs.len()).map(LifecycleIntervention::Collect));
    interventions
}

fn empty_usage(metadata_num: u64) -> Usage {
    Usage {
        current_metadata_size: metadata_num * 192,
        total_metadata_size: metadata_num * 192,
        current_metadata_num: metadata_num,
        total_metadata_num: metadata_num,
        current_content_size: 0,
        total_content_size: 0,
        current_payload_num: 0,
        total_payload_num: metadata_num,
        missing_payload_size: 0,
        missing_payload_num: 0,
        deleted_payload_size: 0,
        deleted_payload_num: 0,
        pruned_payload_size: 0,
        pruned_payload_num: 0,
        invalid_payload_size: 0,
        invalid_payload_num: 0,
    }
}

fn model(specs: &[TargetSpec], materialized: &Materialized) -> LifecycleSnapshot {
    let author = materialized.events[0].author();
    let mut states = BTreeMap::new();
    let mut available = BTreeSet::new();
    let mut reference_counts = BTreeMap::new();
    let mut usage = empty_usage(materialized.events.len() as u64);

    for (index, (spec, event)) in specs
        .iter()
        .zip(&materialized.events[..materialized.target_count])
        .enumerate()
    {
        let id = event.event_id().to_short();
        let len = u64::from(event.content_len());
        usage.total_content_size += len;
        let state = if let Some(deleter) = materialized.deleters.get(&index) {
            usage.deleted_payload_size += len;
            usage.deleted_payload_num += 1;
            SemanticState::Deleted(*deleter)
        } else if spec.pruned {
            usage.pruned_payload_size += len;
            usage.pruned_payload_num += 1;
            SemanticState::Pruned
        } else {
            usage.current_content_size += len;
            usage.current_payload_num += 1;
            available.insert(id);
            *reference_counts.entry(event.content_hash()).or_default() += 1;
            SemanticState::Processed
        };
        states.insert(id, state);
    }

    let deletion_count = materialized.deleters.len() as u64;
    usage.current_payload_num += deletion_count;
    if let Some(deletion) = materialized.events.get(materialized.target_count) {
        reference_counts.insert(deletion.content_hash(), deletion_count);
    }

    LifecycleSnapshot {
        states,
        available,
        reference_counts,
        fetch_queue: BTreeSet::new(),
        usage: BTreeMap::from([(author, usage)]),
    }
}

async fn snapshot(db: &Database, materialized: &Materialized) -> DbResult<LifecycleSnapshot> {
    let target_ids = materialized.events[..materialized.target_count]
        .iter()
        .map(|event| event.event_id().to_short())
        .collect::<Vec<_>>();
    let mut available = BTreeSet::new();
    for event_id in &target_ids {
        if db.get_event_content(*event_id).await.is_some() {
            available.insert(*event_id);
        }
    }
    db.read_with(|tx| {
        let state_table = tx.open_table(&events_content_state::TABLE)?;
        let states = target_ids
            .iter()
            .map(|event_id| {
                let state = match state_table.get(event_id)?.map(|entry| entry.value()) {
                    None => SemanticState::Processed,
                    Some(EventContentState::Deleted { deleted_by }) => {
                        SemanticState::Deleted(deleted_by)
                    }
                    Some(EventContentState::Pruned) => SemanticState::Pruned,
                    Some(state) => panic!("terminal fence left target in {state:?}"),
                };
                Ok((*event_id, state))
            })
            .collect::<DbResult<_>>()?;
        let reference_counts = tx
            .open_table(&content_rc::TABLE)?
            .range(..)?
            .filter_map(|entry| match entry {
                Ok((key, value)) if value.value() != 0 => Some(Ok((key.value(), value.value()))),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_, _>>()?;
        let fetch_queue = tx
            .open_table(&events_content_missing::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value().1))
            .collect::<Result<_, _>>()?;
        let usage = tx
            .open_table(&ids_data_usage::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, value)| (key.value(), Usage::from(value.value()))))
            .collect::<Result<_, _>>()?;
        Ok(LifecycleSnapshot {
            states,
            available,
            reference_counts,
            fetch_queue,
            usage,
        })
    })
    .await
}

async fn check(input: (Vec<TargetSpec>, Plan, Plan)) -> Result<(), String> {
    let (specs, first_plan, second_plan) = input;
    let materialized = materialize(&specs);
    let expected = model(&specs, &materialized);
    let interventions = interventions(&specs);
    let self_id = RostraIdSecretKey::from_bytes([39; 32]).id();
    let replicas = run_pair_observed(
        self_id,
        &materialized.events,
        &interventions,
        &LifecycleRollbackOracle,
        &first_plan,
        &second_plan,
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
            "lifecycle mismatch\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
        ));
    }
    Ok(())
}

/// Delete, prune, and eligible byte collection converge semantically.
#[test]
fn prop_terminal_content_lifecycle_converges() {
    run_property(
        concat!(
            module_path!(),
            "::prop_terminal_content_lifecycle_converges"
        ),
        8,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/lifecycle.txt"
        ),
        strategy(),
        check,
    );
}
