use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use rostra_core::event::{Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_core::{ContentHash, ShortEventId};

use super::runner::{Plan, plan_strategy, run_pair, run_property, with_content};
use crate::{
    Database, DbResult, content_rc, content_store, events_content_missing, events_content_state,
    ids_data_usage,
};

#[derive(Clone, Debug)]
struct ContentSpec {
    author: u8,
    payload: u8,
    byte: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Usage {
    current_metadata_size: u64,
    total_metadata_size: u64,
    current_metadata_num: u64,
    total_metadata_num: u64,
    current_content_size: u64,
    total_content_size: u64,
    current_payload_num: u64,
    total_payload_num: u64,
    missing_payload_size: u64,
    missing_payload_num: u64,
    deleted_payload_size: u64,
    deleted_payload_num: u64,
    pruned_payload_size: u64,
    pruned_payload_num: u64,
    invalid_payload_size: u64,
    invalid_payload_num: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct ContentSnapshot {
    unprocessed: BTreeSet<ShortEventId>,
    fetch_queue: BTreeSet<ShortEventId>,
    store: BTreeMap<ContentHash, Vec<u8>>,
    reference_counts: BTreeMap<ContentHash, u64>,
    usage: BTreeMap<RostraId, Usage>,
}

fn strategy() -> impl Strategy<Value = (Vec<ContentSpec>, Plan, Plan)> {
    (
        prop::collection::vec(
            (0u8..3, 0u8..5, any::<u8>()).prop_map(|(author, payload, byte)| ContentSpec {
                author,
                payload,
                byte,
            }),
            1..=8,
        ),
        plan_strategy(),
        plan_strategy(),
    )
}

fn payload(spec: &ContentSpec) -> EventContentRaw {
    EventContentRaw::new(match spec.payload {
        0 => vec![],
        1 => vec![0x41],
        2 => vec![0x42, 0x42],
        3 => vec![spec.byte],
        _ => vec![spec.byte; usize::from(spec.byte % 8) + 1],
    })
}

fn materialize(specs: &[ContentSpec]) -> Vec<rostra_core::event::VerifiedEventContent> {
    let secrets = [
        RostraIdSecretKey::from_bytes([21; 32]),
        RostraIdSecretKey::from_bytes([22; 32]),
        RostraIdSecretKey::from_bytes([23; 32]),
    ];
    specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let secret = secrets[usize::from(spec.author) % secrets.len()];
            let content = payload(spec);
            let event = Event::builder_raw_content()
                .author(secret.id())
                .kind(EventKind::RAW)
                .content(&content)
                .timestamp(
                    time::OffsetDateTime::from_unix_timestamp(20_000 + index as i64)
                        .expect("property timestamp"),
                )
                .build();
            let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
                .expect("deterministic event verifies");
            with_content(event, content)
        })
        .collect()
}

fn model(events: &[rostra_core::event::VerifiedEventContent]) -> ContentSnapshot {
    let mut store = BTreeMap::new();
    let mut reference_counts = BTreeMap::new();
    let mut usage: BTreeMap<RostraId, Usage> = BTreeMap::new();
    for event in events {
        let content = event.content.as_ref().expect("runner content");
        let hash = content.compute_content_hash();
        store.insert(hash, content.as_ref().to_vec());
        *reference_counts.entry(hash).or_default() += 1;
        let author_usage = usage.entry(event.author()).or_insert(Usage {
            current_metadata_size: 0,
            total_metadata_size: 0,
            current_metadata_num: 0,
            total_metadata_num: 0,
            current_content_size: 0,
            total_content_size: 0,
            current_payload_num: 0,
            total_payload_num: 0,
            missing_payload_size: 0,
            missing_payload_num: 0,
            deleted_payload_size: 0,
            deleted_payload_num: 0,
            pruned_payload_size: 0,
            pruned_payload_num: 0,
            invalid_payload_size: 0,
            invalid_payload_num: 0,
        });
        author_usage.current_metadata_size += 192;
        author_usage.total_metadata_size += 192;
        author_usage.current_metadata_num += 1;
        author_usage.total_metadata_num += 1;
        author_usage.current_content_size += content.len() as u64;
        author_usage.total_content_size += content.len() as u64;
        author_usage.current_payload_num += 1;
        author_usage.total_payload_num += 1;
    }
    ContentSnapshot {
        unprocessed: BTreeSet::new(),
        fetch_queue: BTreeSet::new(),
        store,
        reference_counts,
        usage,
    }
}

async fn snapshot(db: &Database) -> DbResult<ContentSnapshot> {
    db.read_with(|tx| {
        let unprocessed = tx
            .open_table(&events_content_state::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<_, _>>()?;
        let fetch_queue = tx
            .open_table(&events_content_missing::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value().1))
            .collect::<Result<_, _>>()?;
        let store = tx
            .open_table(&content_store::TABLE)?
            .range(..)?
            .map(|entry| {
                entry
                    .map(|(key, value)| (key.value(), value.value().0.as_ref().as_slice().to_vec()))
            })
            .collect::<Result<_, _>>()?;
        let reference_counts = tx
            .open_table(&content_rc::TABLE)?
            .range(..)?
            .filter_map(|entry| match entry {
                Ok((key, value)) if value.value() != 0 => Some(Ok((key.value(), value.value()))),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_, _>>()?;
        let usage = tx
            .open_table(&ids_data_usage::TABLE)?
            .range(..)?
            .map(|entry| {
                entry.map(|(key, value)| {
                    let value = value.value();
                    (
                        key.value(),
                        Usage {
                            current_metadata_size: value.current_metadata_size,
                            total_metadata_size: value.total_metadata_size,
                            current_metadata_num: value.current_metadata_num,
                            total_metadata_num: value.total_metadata_num,
                            current_content_size: value.current_content_size,
                            total_content_size: value.total_content_size,
                            current_payload_num: value.current_payload_num,
                            total_payload_num: value.total_payload_num,
                            missing_payload_size: value.missing_payload_size,
                            missing_payload_num: value.missing_payload_num,
                            deleted_payload_size: value.deleted_payload_size,
                            deleted_payload_num: value.deleted_payload_num,
                            pruned_payload_size: value.pruned_payload_size,
                            pruned_payload_num: value.pruned_payload_num,
                            invalid_payload_size: value.invalid_payload_size,
                            invalid_payload_num: value.invalid_payload_num,
                        },
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(ContentSnapshot {
            unprocessed,
            fetch_queue,
            store,
            reference_counts,
            usage,
        })
    })
    .await
}

async fn check(input: (Vec<ContentSpec>, Plan, Plan)) -> Result<(), String> {
    let (specs, first_plan, second_plan) = input;
    let events = materialize(&specs);
    let expected = model(&events);
    let self_id = RostraIdSecretKey::from_bytes([29; 32]).id();
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
            "content mismatch\nexpected={expected:#?}\nfirst={first:#?}\nsecond={second:#?}"
        ));
    }
    Ok(())
}

/// Live RAW payload storage, RC, state, and usage converge.
#[test]
fn prop_live_raw_content_lifecycle_converges() {
    run_property(
        concat!(
            module_path!(),
            "::prop_live_raw_content_lifecycle_converges"
        ),
        16,
        file!(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/property/content.txt"
        ),
        strategy(),
        check,
    );
}
