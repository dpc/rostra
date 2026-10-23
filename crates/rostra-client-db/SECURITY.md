# Client database security and reliability

SocialPost mention indexing parses remote-authored Djot only after the shared
`rostra-djot` source-size and conservative recursive-block-opener guard succeeds.
Over-budget posts remain eligible for normal signed-event/content ingestion, but
`contains_mention` returns false and their mention index/notification may be
omitted. The budget counts siblings and apparent literal-code openers as well as
nesting, so this includes more than 64 flat list items. It is not a validity rule
for signed content, nor protection for separate unguarded web/excerpt renderers.

Managed post publication atomically checks self-head membership and ingests a
candidate whose previous parent is that exact supplied head. A null expectation
requires both empty self-heads and a root candidate. Do not separate this check
from ingestion or use the independent minimum-head selector: sibling heads and
concurrent requests otherwise break one-use head consumption. Both managed
head consumption and durable request receipts use the same transaction-local
admission helper; failure must not retain a candidate envelope.

Local text-post idempotency uses the
[durable request contract](docs/social-post-idempotency.md).
`commit_social_post_request` trusts its in-process publisher to supply exactly
`request.to_social_post()` as the verified candidate content; it deliberately
does not rebind that supplied payload to the input digest. Network or untrusted
callers must not use this seam. Receipt lookup and admitted ingestion share one
transaction, including envelope reservation; never substitute the separate
header-committing `reserve_payload` call. Receipts survive payload deletion,
pruning and migration, have unbounded aggregate growth outside payload quotas,
and expose metadata and dictionary guesses as described in the linked contract.

Own-text edits/deletes use the separate
[mutation request contract](docs/social-post-mutations.md). Reconcile original
receipts before validating current targets, then bind supported candidate fields
to exact requested text and preserved metadata in the same conditional writer
transaction. Never infer a tombstone from a missing projection plus delete flag.

Independent Add/Remove follows the [reaction request contract](docs/reactions.md).
Keep full target/author binding, selected-row ownership/canonical checks, candidate
payload/nonce binding, admission and receipts in one transaction. The
`social_reaction_receipts` table stays outside extension access and survives
replay. Nonzero request-derived nonsingleton auxiliary keys keep distinct
same-head/same-second adds independent; they do not change singleton or replacement
semantics. Exact materialized summaries fail closed at explicit work limits.

Direct-message source tables additionally contain plaintext history and live
native age identities. Never debug-format these records or export epoch secrets.
Schema32 migration must preserve all four authoritative tables through typed,
source-version-gated stashes; keys/history/tombstones cannot be reconstructed from
ciphertext. Do not perform lifecycle work while any pending authoritative stash
exists. Expiration commits before queued trials, including after restart and
retirement. Assigned deadlines never change, and deleted secrets are never
regenerated. This is live-state removal, not forensic erasure of pages, swap,
dumps or backups. Plaintext history remains retained independently.

Production lifecycle clocks are sampled after acquiring the database writer.
The trial transaction samples again after the separate purge commit: newly
expired keys force another purge, not decryption. Selected send permits retain
the intersection of all chosen keys' sending intervals and publication-skew
bounds; the atomic outgoing writer rechecks this interval after any lock wait.
It also rechecks every selected authoritative device state and the sending
installation: retirement, supersession or reenrollment during encryption cannot
commit a stale selection.
The local installation must also remain unretired: retirement rejects both
previously selected permits and new selections until explicit reenrollment.

History pages use the derived conversation/time index, never a full plaintext
table scan. Device selection uses a derived disjoint dyadic eligibility index:
at most 65 prefix seeks and nine rows per prefix per account, with authoritative
rank/eligibility checks. Old covers are removed atomically with reducer updates;
both indexes rebuild from preserved authoritative state, not available ciphertext.
Never prune expired latest state or retirement tombstones to optimize selection.

Only bounded background work may try decryption. Recheck signed author, kind,
zero aux/non-singleton, hash, length, complete profile and encrypted participants.
Resume all live identities in later eight-key batches; no timestamp-based key
filter or first-batch unreadable decision is allowed. Deduplicate by full sender
and random message ID, preserve the original winner and flag authenticated body
conflicts. No decryption result emits networking or receipts. Outgoing history
must share the admitted event's transaction and commit-before-head boundary.
The DB API trusts in-process callers; session-specific access remains mandatory
even when another session has activated the same account's client.

`rostra-client-db` is the persistent authoritative graph and projection store
for one local Rostra identity. The asynchronous client opens it at startup; one
in-process mutex serializes writes and post-commit publication. The database
file contains signed public events and payloads plus sensitive local identity
and network-key metadata. Filesystem access is trusted and must be limited to
the account running Rostra.

Peer envelopes and payloads are untrusted until normal ingestion verifies event
signatures, event identity, payload hash, and payload length. Retained migration
source rows are trusted as previously admitted data; total replay is not a
cryptographic audit. Caller extension tables are trusted in-process state
outside core replay invariants and remain the extension owner's compatibility
responsibility.

The SocialPost materialization feed stores event identities, not payload copies.
A scan therefore trusts retained events, lifecycle tables, replacement metadata,
and the content store as one coherent snapshot. Missing events, impossible
Missing/Invalid state, absent current bytes, invalid processed content, or
sequence gaps fail closed without returning a checkpoint. Deleted, pruned, or
replaced content is an expected `Removed` result.
For a replaced post, the scan validates coherent lifecycle before returning
`Removed`; if lifecycle claims it remains processed, retained bytes must also be
present and decodable. Replacement metadata does not mask corruption.

The public self-follow snapshot scans the complete encoded self-identity key
prefix through its exclusive lexicographic successor, then exactly decodes every
matching key and value. This prevents malformed self-owned rows with truncated
or trailing followee bytes from falling outside well-formed tuple bounds and
silently producing a partial snapshot. Rows under adjacent identity prefixes
remain outside the snapshot.

Schema 25 performs a forward-only total rebuild. Preparation atomically stashes
retained source and installs the current schema; replay and stash cleanup commit
atomically. Any replay error leaves the complete stash retryable, and code must
detect an existing stash before preparing another migration. Historical source
encodings require fixtures using the corresponding released layouts. A
malformed authoritative stash must fail closed; disposable metadata corruption
must not destroy authoritative source.

Schema 27 also preserves local retention origins and quota decisions as
authoritative source. Their stash tables are mandatory for a schema-27 replay;
missing tables or malformed values fail closed and retain the retryable stash.
Older events have unknown origins rather than fabricated migration-time grace.
The explicit quota transition rejects local-authored payloads, state-bearing
content kinds (such as follow/profile/vote), unknown kinds, unknown origins,
nonexpired materialization grace and untrusted clocks.
Missing admission rejection requires a known, nonfuture header origin but has
not yet started materialization grace. Clock trust is an explicit caller
assertion; the approved runtime assumption trusts the system wall clock, including
startup. No special acknowledgement or clock-monitor subsystem is required.
Unknown legacy origins and existing future/grace checks remain protected.
Automatic destructive work requires explicit immutable Enforce startup
configuration; Disabled is the default and DryRun performs no writes.

The shared admission boundary is installed before publishing an account, client or
HTTP manager. DryRun keeps actual admission Disabled and owns separate forecast
ceilings. No live setter exists. Changes to activation or acquisition paths require
a combined security/reliability audit, not merely constructor tests.
Configured admission rechecks ready accounting and author/database current usage
plus pending logical reservations inside the materialization writer transaction.
One full EventId has one logical acquisition owner; each racing payload attempt
requires its own buffer guard. Committed materialization/terminal changes release
logical ownership after commit, while buffers remain charged until their owners
drop. Cancellation drops unfinished ownership; rollback never publishes a
terminal release.

Enforce acquisition invokes crate-private pending-demand registration and
one-victim preemption through its retained worker. Metadata-only
weak/RAII ownership deduplicates full event IDs, expires after a nonrenewable
30 seconds, and independently bounds intent count/bytes by explicit acquisition
limits without promising reservations or owning buffers. The primitive selects
one live demand rather than summing competing intents; it rechecks Missing rank,
generation, fixed-time due-prefix exhaustion, author/global pressure including
reservations, and current lower-ranked candidate ownership in the same writer
transaction as checked pruning. It never collects bytes. Per-call candidate
visits and logical bytes are bounded, with at most one indivisible reduction and
cooperative time checks; `NoVictim`/`NotReady`/`Bounded` are not busy-loop signals.
One constant-size cursor per bounded intent skips rejected rows across turns;
`Continue` permits a yielded continuation, not an internal loop. Scope changes,
backwards time, skipped-row eligibility and any relevant index mutation invalidate
advice. Mutation revision increments precede index changes, so rollback can only
force extra rescanning. Cursor validity never replaces current transaction checks.
Exhausted/byte-blocked plans permit alternate authors before eviction starts;
afterwards one full event/lease identity retains priority until reservation,
cancellation, expiry or invalidation. This avoids spending released capacity for
aggregate demand. A commit failure can conservatively keep that metadata-only
priority, never grant unchecked authority. Retry walltime is advisory; insufficient
budgets and continuous mutation do not have an unconditional liveness guarantee.

The lock order is DB writer, demand arbitration, then admission state. Logical
lease drops take arbitration before state, preventing disappearing reservation
pressure while pruning; buffer-only releases take state alone. Pruning releases
state before reducers reacquire it, holds arbitration through the reducer, and
releases both before commit hooks. Cancellation linearizes under arbitration,
before or after that reduction. Normal internal entry points sample walltime only
after acquiring writer and arbitration, so lock waits cannot preserve expired
pre-lock authority. They reuse that timestamp for expiry, due-prefix, rank and
checked reduction. Completion, reservation, reattachment and policy replacement
invalidate demands. See the [database guide](../../docs/payload-retention-database.md)
and [startup audit](../../docs/payload-retention-startup.md) for the scope.

The configured runtime connects ordinary acquisition preparation
to metadata-only demand waiting and a bounded maintenance/preemption driver.
It independently maintains accounting,
quota-nomination recovery, candidate backfill and fixed-time grace prefixes, with
fresh writer-time authority checks for every prune. Explicit per-turn operation,
logical-byte and cooperative-time bounds do not preempt an indivisible database
operation. Waits own no payload buffers, deduplicate without renewing intent and
have a monotonic outer deadline. Register-before-check notifications, minimum retry
spacing and recovery waits avoid relying on a lossy wakeup or immediately retrying
unattainable byte allowances.

The same demand boundary can durably reject eligible Missing content only against
a fresh, currently eligible retained index head with a strictly higher full rank,
under retained-only cap pressure independent of reservations. Protected bytes count
against caps but do not alone establish a boundary. Fits/exact partial-plan barriers
and live incoming reservations block rejection; skipped/future heads and unready
state remain Deferred. The checked Missing reducer removes retry scheduling and
preserves replay decisions without releasing retained logical bytes. Duplicate
headers, late content and shared-store reuse cannot resurrect that terminal state.

The runner spawns nothing; its caller must own and join its future. Unique RAII
runner ownership releases on cancellation/panic, after any synchronous transaction
finishes. Client retains the worker before ingress/signing task startup, including
when optional replication is disabled. The manager retains all task completions
before construction; errors, panic and cancelled join waiters cannot bypass actual
old-task termination, including DB-less tasks, before reconstruction or reopen.

The DryRun branch owns separate immutable forecast ceilings and requires
Disabled, empty admission ownership; it never installs enforcing acquisition caps.
Preparation and ingestion retain Disabled behavior. Its bounded whole-source read
snapshot performs no writes at all: no lifecycle/projection/counter/source changes,
derived maintenance, demand mutation, rejection or GC. Accounting-unready and
count/author/logical-byte/time overflow produce no projection. Complete reports are
retained-only fresh-start author-first/global90% projections, not Enforce forecasts
with reservations, demand, future workload or inherited hysteresis. Reports replace
as-of observations rather than accumulate fresh victims; one attempt per monotonic
second bounds repeated scanning. Model memory is count-bounded, no payload bodies
are read, and sorting/DB operations are indivisible under cooperative time limits.
Large databases can remain incomplete indefinitely. Guarded ledger zeroes under
Disabled do not measure actual acquisition memory. Neither logical victims nor
observed unique-store bytes promise physical reclamation.

The Enforce driver includes general author-first/global pressure on retained plus
logically reserved bytes, with experimental floor90% low-water hysteresis.
Schema31 stores disposable epoch-scoped author targets/frontiers and a global
target, not replay authority. A new runtime ignores previous-incarnation targets
and cleans stale rows during bounded author discovery. A completed author sweep
cannot authorize global pruning after logical usage/lease or candidate mutation,
an author eligibility deadline, or backwards walltime. Saturated mutation revisions
fail shut; aborted mutations may conservatively invalidate advice. Invalidation
waits rather than spinning under churn; arbitrary sustained writes can delay a
complete sweep.

General reduction uses the same writer → demand arbitration → state lock order,
fresh clock, current generation/config/accounting/reservation/eligibility checks
and checked reducer. Fits and exact active event/lease partial-plan barriers
prevent spending acquisition-freed room twice; unrelated nonfitting intent does
not suppress general relief. State is dropped before the reducer reacquires it,
while arbitration excludes cancellation/logical release through reduction.

Quota-only collection shares operation/time bounds and has a separate strict
unique-store-value **bytes removed** allowance. An exclusive hash cursor retains
oversized nominations, visits later smaller work, then waits after a full sweep
before retrying. RC/history/provenance checks remain transactional. Protected or
oversized values may remain forever; unrelated legacy/signed-delete garbage is
not newly nominated. Neither logical eviction nor collection claims physical-page
reclamation, whole-process RAM bounds, or hard time bounds for indivisible DB
operations.

The buffer limit is declared capacity, not a whole-process memory measurement.
Existing APIs' already-allocated external content, transport/codec working memory,
allocator overhead and clones retained after acquisition remain outside its
guarantee. Client acquisition now reserves before reads, including a separate
full-payload Vec-to-Arc conversion charge, and retains the winner through ingestion.
Raw signed HTTP uses a pinned body limit and consults immutable startup account
ownership before parsing, falling back to an already-loaded client's ledger.
Local serialization charges output growth before allocation. Unverified HTTP
paths never create/open a database. Explicitly listed account ledgers survive
lazy loading and have exclusive database attachment; concurrent manager loads are
serialized. Reattachment invalidates old logical leases but preserves outstanding
buffer charges. Unlisted HTTP identities cannot grow the account registry.
Client-cache eviction preserves discoverability of still-live clients or storage.
Database keepers are released through atomic sole-ownership cleanup before fresh
opens, not a race-prone check of reference counts or failed weak upgrades.
Explicitly listed Enforce accounts install validated preparse capacity before
publishing the manager. Five 2-MiB slots cover raw-body/string/scratch/conversion
overlap; insufficient capacity refuses before parsing and releases partial owners.
No hot reload API exists; mode/budget/
policy changes require fresh construction after all old work and guards quiesce.
The body limit alone does not bound aggregate memory for these disabled requests.
Low-level
P2P callers still own their allocation policy; supplying a dummy guard is not a
supported client admission path. Adding any acquisition path or changing buffer
representations is a capacity-audit trigger.

Changes to ranking-aware admission, Client worker ownership or startup configuration
must preserve the complete enabled-mode ingress audit and independently reviewed
writer authority, not rely on isolated checkpoint reviews. Protecting local/state/
unknown content from eviction does not exempt it from admission caps. Runtime
clock policy trusts the system wall clock, per the operator decision; unknown
legacy origins and existing future/grace checks remain conservative.

Admission notifications are lossy: callers must register before checking work,
then recheck, and also wake for startup, configuration, accounting readiness and
bounded retry/grace deadlines. Deferred Missing work must not spin on its queue
row or treat temporary pressure as a peer failure. Primitive tests alone do not
prove combined worker liveness or complete ingress coverage. Enabled startup,
Client lifecycle and real HTTP fixtures supplement the race/overload suites;
stable-input conditional liveness and declared acquisition scope remain limits.

Runtime progress is phase-local: reset the phase result before every phase and
accumulate only fresh work in the current seven-phase cycle. Never feed completed
cycle progress back into `cycle_progress` across cooperative deadline splits.
That can prevent the idle `Wait` boundary indefinitely, leaving pressure and GC
waiting flags unreconciled after a successful prune invalidates advisory state.
`Wait` resets those recovery flags; it is not proof that a pressure target was
met. Scheduler changes must preserve
`runtime_multi_cycle_turns_do_not_recycle_old_progress`, which requires target
attainment, an actual wait and GC completion across multi-cycle turns. Revisit
the [disposable disk evaluation](../../docs/payload-retention-evaluation.md#preserved-pre-fix-failures-and-diagnosis)
when changing phase scheduling or time/operation budgeting: tiny fixed-turn
fixtures alone missed a stable-input disk plateau under variable deadline splits.

Schema 28 adds disposable global logical/unique-byte accounting and a quota-only
physical collector. All databases start unready; totals remain unavailable and
collection fails closed until an explicit bounded rebuild finishes. The rebuild
derives expected RC (including Missing references) and current logical usage
from retained events, checks expected/actual RC and both author indexes, and
counts actual unique store values. It rejects mismatches and absent Processed
bytes. Cursor-covered ingestion deltas, cursor advancement and partial totals
commit transactionally, so batches can interleave with normal writes and resume
after interruption without exposing partial readiness.

For a quota-nominated hash, a retained local-authored or non-SocialPost header is
a conservative historical protection guard even after it releases event RC.
Under the current protected-kind policy the guard has no release operation.
It is not RC or current logical usage; it can prevent physical reclamation
indefinitely. Before removing any nominated bytes, the collector rechecks
readiness, expected/actual RC, zero references and the historical guard inside
the write transaction. Store removal, unique-byte decrement and nomination
consumption are atomic. Blocked nominations are consumed without removal and
are requeued on a later final reference release only when historical quota-hash
provenance authorizes that work.

The checked quota transition nominates atomically with dematerialization,
lifecycle, usage and reference changes, and emits invalidation only after commit.
It preserves canonical edit/deletion lineage and immutable feed rows, never
representing local pruning as author deletion. Errors abort every side effect.
General garbage from signed deletion, invalidation and legacy size pruning is
outside this collector's scope. Total replay drops disposable accounting and the nomination
queue/provenance/recovery cursor, then requires explicit rebuilds. Schema 29's
separate bounded quota-row scan reconstructs provenance and pending work;
concurrent quota transitions nominate directly, while final reference releases
requeue scanned hashes and unscanned rows nominate when visited. The recovery
cursor and reconstructed rows commit together and resume after reopen.
Accounting readiness alone does not mean nomination recovery is complete.
Existing replay may omit unreferenced Deleted bytes; rebuilt totals describe the
actual store. Canonical SocialPost replacement lineage survives independently of those bytes. Rebuilt
historical guards protect surviving or reintroduced shared collisions, not a
promise that deleted bytes survive replay.

Maintenance limits bound visited rows, not payload bytes, aggregate retained
storage, index overhead or redb file allocation. Accounting readiness does not
prove readiness of a future policy candidate generation, and neither stored
wall-clock observations nor static ranking keys authorize quota pruning.

Replay runs before the database is published, suppresses incremental hooks and
materialization-feed emission, and refreshes current-state watches after commit.
Total migration preserves a feed from schema 26 or newer byte-for-byte; older
schemas and the version-26 cutover create an empty feed without reconstructing
historical occurrences. Replay does not retain an event graph
or per-event publication closures. It transiently owns decoded payload and
codec copies, then allocates the final follow/follower/WoT snapshot. The redb
backend also tracks dirty, allocated, and freed pages for the whole atomic
transaction, so total process memory is not constant in database size.

Operators must back up the database before upgrade and provision measured RAM
and temporary disk headroom for their database shape. There is no preflight or
safe fixed free-space multiplier. Once preparation commits schema 25, an older
binary cannot open the database; rollback means restoring the pre-upgrade
backup. Disk exhaustion or interruption may leave a roll-forward stash and
require retry with schema-25 code. Replay does not compact automatically.

Primary safeguards are atomic redb transactions, retry-marker persistence,
strict historical value decoding, content commitment verification,
identity-collision rejection, and historical-layout migration tests. Re-check
these safeguards whenever changing retained source formats, schema/version
handling, migration markers, decoding, transaction boundaries, or replay
publication.

The current redb-bincode range API validates encoded keys infallibly, and
migration value decoding does not impose a separate allocation limit on corrupt
length prefixes. Because the database file and local filesystem are trusted,
malformed authoritative keys or hostile encoded lengths are an accepted
non-adversarial corruption risk: open may panic or exhaust memory while the
committed stash remains on disk. Restore the pre-upgrade backup or use an
offline audited repair tool; repeated normal opens are not a corruption scrub.
