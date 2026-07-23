# Client database convergence properties

`src/tests/property/` checks durable database semantics under independently
generated delivery schedules. Each case materializes deterministic signed input,
copies one closed database template to two replicas, and varies permutation,
duplicates, transaction batches, aborted transactions, split versus atomic
envelope/content delivery (including content carrying an absent envelope), and
intermediate reopen points. Lifecycle properties also schedule explicit pruning
and eligible zero-reference byte collection through the same batching, abort,
duplicate, and reopen machinery. Property-specific snapshots verify immediately
that aborted batches retain no modeled lifecycle or projection state. A final
envelope retry, payload retry, intervention retry, and reopen form the
quiescence fence.

The properties cover the invariants governed by
[`SPEC-event-graph`](../../rostra-core/specs/SPEC-event-graph.md),
[`ARCH-client-database`](../specs/ARCH-client-database.md), and
[`SPEC-event-content-lifecycle`](../specs/SPEC-event-content-lifecycle.md):

- author-scoped envelope graph state, including cross-author raw parent matches
  and canonical deletion attribution for unresolved parents;
- live RAW payload state, exact content bytes, normalized nonzero reference
  counts, terminal queue emptiness, and complete usage accounting;
- deletion, explicit pruning, and eligible byte collection, using effective
  per-event state and availability rather than zero-reference store residue.
  Successful zero-reference removal is intentionally not itself an oracle;
  collection attempts against shared live bytes must preserve availability;
- social-post replacement lineage composed with symmetric authored-time, reply,
  news, self-mention, receipt-membership, and visibility reversion;
- strict-time and equal-time follow, profile, generic singleton, and vote
  reducers, split into three properties for useful shrinking.

Each property compares both replicas with an independently computed semantic
model. It decodes typed tables into maps and sets rather than comparing database
pages or deriving expected state from the final database.

The properties intentionally exclude local receipt ordering, request clocks,
watch/broadcast scheduling, session authority, fetch retry timing and attempt
metadata, physical zero-reference garbage-collection residue, and
absent-versus-zero vote sum rows. Terminal fetch-queue emptiness and semantic
availability after eligible collection are covered. The excluded values either
are intentionally local, require a different concurrency oracle, or are
permitted physical representations of the same lifecycle state.

Normal test runs use small disk-specific case budgets. `PROPTEST_CASES` overrides
the budget separately for every selected property while preserving proptest
shrinking and failure persistence. The following full-family command is a long
soak—2,000 cases for each property, not 2,000 cases in total:

```sh
PROPTEST_CASES=2000 cargo test -p rostra-client-db 'property::' -- --nocapture
```

Prefer a targeted soak while investigating one family:

```sh
PROPTEST_CASES=200 cargo test -p rostra-client-db \
  'property::lifecycle::prop_terminal_content_lifecycle_converges' -- --nocapture
```

Total replay remains in focused migration and lifecycle regression tests; the
shared property runner does not model it. Append-only post/reply pagination,
failed-fetch queue/CAS behavior, and receipt-index uniqueness as a standalone
local invariant remain possible future families. Evidence from the two
prioritized lifecycle families should justify adding their disk runtime rather
than expanding the default suite automatically.
