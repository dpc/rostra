use std::collections::BTreeSet;
use std::path::Path;

use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use rostra_core::event::{VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use tempfile::{TempDir, tempdir};

use crate::{Database, DbResult, WriteTransactionCtx, events};

const EVENT_PRIORITY_SLOTS: usize = 48;
const EVENT_DUPLICATE_SLOTS: usize = 16;
const EVENT_BATCH_SLOTS: usize = 48;
const TOTAL_PRIORITY_SLOTS: usize = 72;
const TOTAL_DUPLICATE_SLOTS: usize = 24;
const TOTAL_BATCH_SLOTS: usize = 72;

/// A shrinkable delivery plan independent from the generated semantic input.
#[derive(Clone, Debug)]
pub(super) struct Plan {
    /// Priority keys that permute actions.
    priorities: [u8; TOTAL_PRIORITY_SLOTS],
    /// Per-action duplicate counts.
    duplicates: [u8; TOTAL_DUPLICATE_SLOTS],
    /// Per-event choice between split and atomic delivery.
    atomic: [bool; 8],
    /// Committed or aborted batch widths.
    batch_widths: [u8; TOTAL_BATCH_SLOTS],
    /// Whether each batch is aborted before commit.
    aborts: [bool; TOTAL_BATCH_SLOTS],
    /// Whether each batch boundary closes and reopens the database.
    reopens: [bool; TOTAL_BATCH_SLOTS],
}

impl Plan {
    /// Preserve ordering, duplication, batching, and aborts without reopen
    /// cost.
    pub(super) fn without_reopens(&self) -> Self {
        let mut plan = self.clone();
        plan.reopens.fill(false);
        plan
    }
}

/// Generate a delivery plan without irrelevant intervention-tail values.
pub(super) fn plan_strategy() -> impl Strategy<Value = Plan> {
    (
        any::<[u8; EVENT_PRIORITY_SLOTS]>(),
        any::<[u8; EVENT_DUPLICATE_SLOTS]>(),
        any::<[bool; 8]>(),
        any::<[u8; EVENT_BATCH_SLOTS]>(),
        any::<[bool; EVENT_BATCH_SLOTS]>(),
        any::<[bool; EVENT_BATCH_SLOTS]>(),
    )
        .prop_map(
            |(priorities, duplicates, atomic, batch_widths, aborts, reopens)| Plan {
                priorities: {
                    let mut expanded = [0; TOTAL_PRIORITY_SLOTS];
                    expanded[..EVENT_PRIORITY_SLOTS].copy_from_slice(&priorities);
                    expanded
                },
                duplicates: {
                    let mut expanded = [0; TOTAL_DUPLICATE_SLOTS];
                    expanded[..EVENT_DUPLICATE_SLOTS].copy_from_slice(&duplicates);
                    expanded
                },
                atomic,
                batch_widths: {
                    let mut expanded = [0; TOTAL_BATCH_SLOTS];
                    expanded[..EVENT_BATCH_SLOTS].copy_from_slice(&batch_widths);
                    expanded
                },
                aborts: {
                    let mut expanded = [false; TOTAL_BATCH_SLOTS];
                    expanded[..EVENT_BATCH_SLOTS].copy_from_slice(&aborts);
                    expanded
                },
                reopens: {
                    let mut expanded = [false; TOTAL_BATCH_SLOTS];
                    expanded[..EVENT_BATCH_SLOTS].copy_from_slice(&reopens);
                    expanded
                },
            },
        )
}

/// Generate a plan whose tail can schedule lifecycle interventions.
pub(super) fn intervention_plan_strategy() -> impl Strategy<Value = Plan> {
    (
        any::<[u8; TOTAL_PRIORITY_SLOTS]>(),
        any::<[u8; TOTAL_DUPLICATE_SLOTS]>(),
        any::<[bool; 8]>(),
        any::<[u8; TOTAL_BATCH_SLOTS]>(),
        any::<[bool; TOTAL_BATCH_SLOTS]>(),
        any::<[bool; TOTAL_BATCH_SLOTS]>(),
    )
        .prop_map(
            |(priorities, duplicates, atomic, batch_widths, aborts, reopens)| Plan {
                priorities,
                duplicates,
                atomic,
                batch_widths,
                aborts,
                reopens,
            },
        )
}

/// Configure a disk-backed property while preserving `PROPTEST_CASES`.
pub(super) fn config(
    default_cases: u32,
    source_file: &'static str,
    persistence_file: &'static str,
) -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = default_cases;
    }
    config.source_file = Some(source_file);
    config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(persistence_file)));
    config
}

/// Run one async property with one Tokio runtime for all generated cases.
pub(super) fn run_property<S, F, Fut>(
    name: &'static str,
    cases: u32,
    source_file: &'static str,
    persistence_file: &'static str,
    strategy: S,
    property: F,
) where
    S: Strategy,
    S::Value: std::fmt::Debug,
    F: Fn(S::Value) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("property runtime");
    let mut config = config(cases, source_file, persistence_file);
    config.test_name = Some(name);
    proptest::test_runner::TestRunner::new(config)
        .run(&strategy, |value| {
            runtime
                .block_on(property(value))
                .map_err(TestCaseError::fail)
        })
        .unwrap_or_else(|error| panic!("{name}: {error}"));
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Envelope(usize),
    Content(usize),
    EnvelopeWithContent(usize),
    Intervention(usize),
}

/// A test-only operation composed with ordinary scheduled ingestion.
pub(super) trait Intervention {
    /// Stable tie-break position when generated priorities become equal.
    fn sort_key(&self, index: usize, event_count: usize) -> (usize, u8) {
        (event_count + index, 1)
    }

    /// Apply the operation inside the runner's current transaction.
    fn apply(
        &self,
        db: &Database,
        events: &[VerifiedEventContent],
        tx: &WriteTransactionCtx,
    ) -> DbResult<()>;
}

/// Property-specific state checked immediately around aborted batches.
#[allow(async_fn_in_trait)]
pub(super) trait RollbackOracle {
    /// Comparable state protected by transaction rollback.
    type Snapshot: std::fmt::Debug + Eq;

    /// Read the state whose mutation must not escape an aborted transaction.
    async fn snapshot(
        &self,
        db: &Database,
        events: &[VerifiedEventContent],
    ) -> DbResult<Self::Snapshot>;
}

/// Two replicas initialized from the exact same closed database image.
pub(super) struct ReplicaPair {
    /// First independently scheduled replica.
    pub(super) first: Database,
    /// Second independently scheduled replica.
    pub(super) second: Database,
    /// Keeps the template and replica files alive until both databases close.
    _dir: TempDir,
}

fn actions<I: Intervention>(event_count: usize, interventions: &[I], plan: &Plan) -> Vec<Action> {
    assert!(event_count <= plan.atomic.len());
    assert!(event_count * 2 + interventions.len() <= plan.duplicates.len());
    let mut keyed = Vec::new();
    for index in 0..event_count {
        let base: &[(usize, Action)] = if plan.atomic[index] {
            &[(0, Action::EnvelopeWithContent(index))]
        } else {
            &[(0, Action::Envelope(index)), (1, Action::Content(index))]
        };
        for &(kind, action) in base {
            let logical_slot = index * 2 + kind;
            let duplicate_count = usize::from(plan.duplicates[logical_slot] % 3) + 1;
            for duplicate in 0..duplicate_count {
                let priority_slot = logical_slot * 3 + duplicate;
                keyed.push((
                    (
                        plan.priorities[priority_slot],
                        index,
                        (kind * 2) as u8,
                        duplicate,
                    ),
                    action,
                ));
            }
        }
    }
    for (index, intervention) in interventions.iter().enumerate() {
        let logical_slot = event_count * 2 + index;
        let duplicate_count = usize::from(plan.duplicates[logical_slot] % 3) + 1;
        for duplicate in 0..duplicate_count {
            let priority_slot = logical_slot * 3 + duplicate;
            let (target, kind) = intervention.sort_key(index, event_count);
            keyed.push((
                (plan.priorities[priority_slot], target, kind, duplicate),
                Action::Intervention(index),
            ));
        }
    }
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, action)| action).collect()
}

fn apply_action<I: Intervention>(
    db: &Database,
    events: &[VerifiedEventContent],
    interventions: &[I],
    action: Action,
    now: Timestamp,
    tx: &WriteTransactionCtx,
) -> DbResult<()> {
    match action {
        Action::Envelope(index) => {
            db.process_event_tx(&events[index].event, now, tx)?;
        }
        Action::Content(index) => {
            db.process_event_content_tx(&events[index], now, tx)?;
        }
        Action::EnvelopeWithContent(index) => {
            db.process_event_tx(&events[index].event, now, tx)?;
            db.process_event_content_tx(&events[index], now, tx)?;
        }
        Action::Intervention(index) => {
            interventions[index].apply(db, events, tx)?;
        }
    }
    Ok(())
}

async fn durable_event_ids(db: &Database) -> DbResult<BTreeSet<ShortEventId>> {
    db.read_with(|tx| {
        tx.open_table(&events::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    })
    .await
}

async fn apply_batch<I: Intervention, O: RollbackOracle>(
    db: &Database,
    events: &[VerifiedEventContent],
    interventions: &[I],
    rollback_oracle: &O,
    batch: &[Action],
    abort: bool,
    receipt_counter: &mut u64,
) -> Result<(), String> {
    let durable_before = durable_event_ids(db)
        .await
        .map_err(|error| error.to_string())?;
    let rollback_before = if abort {
        Some(
            rollback_oracle
                .snapshot(db, events)
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let mut known = durable_before.clone();
    let mut applicable = Vec::new();
    for action in batch {
        let index = match action {
            Action::Envelope(index)
            | Action::Content(index)
            | Action::EnvelopeWithContent(index) => *index,
            Action::Intervention(_) => {
                applicable.push(*action);
                continue;
            }
        };
        let event_id = events[index].event_id().to_short();
        match action {
            Action::Content(index) if !known.contains(&event_id) => {
                known.insert(event_id);
                applicable.push(Action::EnvelopeWithContent(*index));
            }
            Action::Envelope(_) | Action::EnvelopeWithContent(_) => {
                known.insert(event_id);
                applicable.push(*action);
            }
            Action::Content(_) => applicable.push(*action),
            Action::Intervention(_) => unreachable!("handled before event lookup"),
        }
    }

    let start = *receipt_counter;
    *receipt_counter += applicable.len() as u64;
    let apply = |tx: &WriteTransactionCtx| -> DbResult<()> {
        for (offset, action) in applicable.iter().enumerate() {
            apply_action(
                db,
                events,
                interventions,
                *action,
                Timestamp::from(1_000_000 + start + offset as u64),
                tx,
            )?;
        }
        Ok(())
    };

    if abort {
        let tx =
            WriteTransactionCtx::from(db.inner.begin_write().expect("begin abort transaction"));
        apply(&tx).map_err(|error| error.to_string())?;
        drop(tx);
        let rollback_after = rollback_oracle
            .snapshot(db, events)
            .await
            .map_err(|error| error.to_string())?;
        let rollback_before = rollback_before.expect("aborted batch snapshot");
        if rollback_after != rollback_before {
            return Err(format!(
                "aborted batch retained modeled state\nbefore={rollback_before:#?}\nafter={rollback_after:#?}"
            ));
        }
        let durable_after = durable_event_ids(db)
            .await
            .map_err(|error| error.to_string())?;
        if durable_after != durable_before {
            return Err(format!(
                "aborted batch retained envelopes\nbefore={durable_before:#?}\nafter={durable_after:#?}"
            ));
        }
        Ok(())
    } else {
        db.write_with(apply)
            .await
            .map_err(|error| error.to_string())
    }
}

async fn reopen(path: &Path, self_id: RostraId, db: Database) -> DbResult<Database> {
    drop(db);
    Database::open(path, self_id).await
}

async fn execute_plan<I: Intervention, O: RollbackOracle>(
    path: &Path,
    self_id: RostraId,
    mut db: Database,
    events: &[VerifiedEventContent],
    interventions: &[I],
    rollback_oracle: &O,
    plan: &Plan,
) -> Result<Database, String> {
    let actions = actions(events.len(), interventions, plan);
    let mut cursor = 0;
    let mut batch_index = 0;
    let mut receipt_counter = 0;
    while cursor < actions.len() {
        let width = usize::from(plan.batch_widths[batch_index] % 4) + 1;
        let end = (cursor + width).min(actions.len());
        apply_batch(
            &db,
            events,
            interventions,
            rollback_oracle,
            &actions[cursor..end],
            plan.aborts[batch_index],
            &mut receipt_counter,
        )
        .await?;
        cursor = end;
        if plan.reopens[batch_index] {
            db = reopen(path, self_id, db)
                .await
                .map_err(|error| error.to_string())?;
        }
        batch_index += 1;
    }

    let envelopes: Vec<_> = (0..events.len()).map(Action::Envelope).collect();
    apply_batch(
        &db,
        events,
        interventions,
        rollback_oracle,
        &envelopes,
        false,
        &mut receipt_counter,
    )
    .await?;
    let contents: Vec<_> = (0..events.len()).map(Action::Content).collect();
    apply_batch(
        &db,
        events,
        interventions,
        rollback_oracle,
        &contents,
        false,
        &mut receipt_counter,
    )
    .await?;
    let intervention_actions: Vec<_> = (0..interventions.len()).map(Action::Intervention).collect();
    apply_batch(
        &db,
        events,
        interventions,
        rollback_oracle,
        &intervention_actions,
        false,
        &mut receipt_counter,
    )
    .await?;
    reopen(path, self_id, db)
        .await
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy)]
struct NoIntervention;

impl Intervention for NoIntervention {
    fn apply(
        &self,
        _db: &Database,
        _events: &[VerifiedEventContent],
        _tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        Ok(())
    }
}

struct NoRollbackOracle;

impl RollbackOracle for NoRollbackOracle {
    type Snapshot = ();

    async fn snapshot(
        &self,
        _db: &Database,
        _events: &[VerifiedEventContent],
    ) -> DbResult<Self::Snapshot> {
        Ok(())
    }
}

/// Apply one finite event set under two independent schedules and final fences.
pub(super) async fn run_pair(
    self_id: RostraId,
    events: &[VerifiedEventContent],
    first_plan: &Plan,
    second_plan: &Plan,
) -> Result<ReplicaPair, String> {
    run_pair_observed(
        self_id,
        events,
        &[] as &[NoIntervention],
        &NoRollbackOracle,
        first_plan,
        second_plan,
    )
    .await
}

/// Apply events and interventions with property-specific abort observations.
pub(super) async fn run_pair_observed<I: Intervention, O: RollbackOracle>(
    self_id: RostraId,
    events: &[VerifiedEventContent],
    interventions: &[I],
    rollback_oracle: &O,
    first_plan: &Plan,
    second_plan: &Plan,
) -> Result<ReplicaPair, String> {
    let dir = tempdir().expect("property tempdir");
    let template_path = dir.path().join("template.redb");
    let first_path = dir.path().join("first.redb");
    let second_path = dir.path().join("second.redb");

    drop(
        Database::open(&template_path, self_id)
            .await
            .map_err(|error| error.to_string())?,
    );
    std::fs::copy(&template_path, &first_path).expect("copy first database template");
    std::fs::copy(&template_path, &second_path).expect("copy second database template");

    let first = Database::open(&first_path, self_id)
        .await
        .map_err(|error| error.to_string())?;
    let second = Database::open(&second_path, self_id)
        .await
        .map_err(|error| error.to_string())?;
    let first = execute_plan(
        &first_path,
        self_id,
        first,
        events,
        interventions,
        rollback_oracle,
        first_plan,
    )
    .await?;
    let second = execute_plan(
        &second_path,
        self_id,
        second,
        events,
        interventions,
        rollback_oracle,
        second_plan,
    )
    .await?;
    Ok(ReplicaPair {
        _dir: dir,
        first,
        second,
    })
}

/// Convert a generated raw envelope into content accepted by the common runner.
pub(super) fn with_content(
    event: VerifiedEvent,
    content: rostra_core::event::EventContentRaw,
) -> VerifiedEventContent {
    VerifiedEventContent::assume_verified(event, content)
}

/// An aborted ingestion batch remains absent after reopening the database.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn aborted_batch_rolls_back_before_retry_fence() {
    use rostra_core::event::{Event, EventContentRaw, EventKind};

    let secret = RostraIdSecretKey::from_bytes([61; 32]);
    let raw = EventContentRaw::new(vec![1, 2, 3]);
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::RAW)
        .content(&raw)
        .build();
    let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))
        .expect("deterministic event verifies");
    let events = vec![with_content(event, raw)];
    let dir = tempdir().expect("abort regression tempdir");
    let path = dir.path().join("abort.redb");
    let db = Database::open(&path, secret.id())
        .await
        .expect("open abort regression database");
    let mut receipt_counter = 0;
    apply_batch(
        &db,
        &events,
        &[] as &[NoIntervention],
        &NoRollbackOracle,
        &[Action::EnvelopeWithContent(0)],
        true,
        &mut receipt_counter,
    )
    .await
    .expect("abort batch");
    let db = reopen(&path, secret.id(), db)
        .await
        .expect("reopen abort regression database");
    assert!(
        durable_event_ids(&db)
            .await
            .expect("read events")
            .is_empty()
    );
}
