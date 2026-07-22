# Client database convergence properties

`src/tests/property/` checks durable database semantics under independently
generated delivery schedules. Each case materializes deterministic signed input,
copies one closed database template to two replicas, and varies permutation,
duplicates, transaction batches, aborted transactions, split versus atomic
envelope/content delivery (including content carrying an absent envelope), and
intermediate reopen points. A final envelope
retry, payload retry, and reopen form the quiescence fence.

The properties cover the invariants governed by
[`SPEC-event-graph`](../../rostra-core/specs/SPEC-event-graph.md),
[`ARCH-client-database`](../specs/ARCH-client-database.md), and
[`SPEC-event-content-lifecycle`](../specs/SPEC-event-content-lifecycle.md):

- author-scoped envelope graph state, including cross-author raw parent matches
  and canonical deletion attribution for unresolved parents;
- live RAW payload state, exact content bytes, normalized nonzero reference
  counts, terminal queue emptiness, and complete usage accounting;
- strict-time and equal-time follow, profile, generic singleton, and vote
  reducers, split into three properties for useful shrinking.

Each property compares both replicas with an independently computed semantic
model. It decodes typed tables into maps and sets rather than comparing database
pages or deriving expected state from the final database.

The properties intentionally exclude local receipt ordering, request clocks,
watch/broadcast scheduling, session authority, fetch retry timing and attempt
metadata, physical garbage-collection residue, replacement lineage, and
absent-versus-zero vote sum rows. Terminal fetch-queue emptiness is covered. The
excluded values either are intentionally local, require a different concurrency
oracle, or belong to a separate lifecycle family.

Normal test runs use small disk-specific case budgets. `PROPTEST_CASES` overrides
those defaults while preserving proptest shrinking and failure persistence:

```sh
PROPTEST_CASES=2000 cargo test -p rostra-client-db 'property::' -- --nocapture
```

Append-only posts/replies and deletion plus pruning/GC lifecycle properties are
future families. They should reuse the schedule runner rather than introduce a
second set of delivery semantics.
