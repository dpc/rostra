# ARCH-client-runtime: Identity client orchestration

`rostra-client` is the runtime boundary for one Rostra identity. It composes
the per-identity database, Pkarr identity discovery, an iroh endpoint, typed
Rostra RPC connections, publication APIs, and synchronization tasks. Its place
in the wider system is described by
[ARCH-rostra](../../../specs/ARCH-rostra.md).

## Modes and authority

A client always has a Rostra identity and a database interface. Supplying
durable storage creates a full client; otherwise the client uses temporary
in-memory storage. Full clients run background replication and projection
maintenance tasks. Both modes can serve incoming requests when configured to
start the request handler.

A client begins read-only unless given or later unlocked with the secret key
matching its identity. Unlocking starts tasks that require signing authority,
including identity-state publication and merging local graph heads.
Read-only operation can resolve peers, receive requests, synchronize known
data, and expose stored state without possessing the identity secret.

## Network composition

Pkarr maps a Rostra identity to its advertised iroh endpoint and a deterministic
representative of its current graph-head set. The representative does not imply
that the set is a singleton. Iroh supplies encrypted peer transport under the
Rostra v0 ALPN.
`rostra-p2p` provides typed RPC connections; the client decides whom to
connect to, caches connections, tracks per-identity and per-node backoff, and
passes verified data to
[ARCH-client-database](../../rostra-client-db/specs/ARCH-client-database.md).
Unsigned routing and admission metadata in a response remains peer-controlled.
When `WAIT_FOLLOWERS_NEW_HEADS` supplies both a claimed author and a signed
event, the client requires the claim to equal the event's cryptographically
verified author before applying Web-of-Trust admission or storing the event.

Relay-only iroh transport is the default privacy mode. Explicit public mode
enables direct IP transports. Published endpoint data is sanitized to avoid
advertising local, loopback, documentation, multicast, and similar unsuitable
addresses.

## Runtime tasks and flows

The request handler serves graph heads, event envelopes, and content to peers.
For a durable client, cooperating tasks react to database notifications and
periodic discovery to:

- discover newer heads for followers and followees;
- fetch absent event parents and payloads;
- synchronize the direct and extended web of trust;
- broadcast local head changes and update derived news scores.

The tasks coordinate through durable database state and subscriptions rather
than owning a second graph state. They must tolerate duplicate wakeups,
temporary peer failure, and out-of-order delivery. Dropping the client requests
cancellation of its background work. Internal multi-client ownership retains
shared join completion so DB-only reconstruction waits until every old task has
actually terminated. Completion is retained before construction starts any task;
a unique abort-on-drop owner moves from partial construction into the client.
Failed or panicked initialization therefore follows the same join-before-retry
rule, including workers that hold no database reference.

Direct single-client hosts can instead initiate irreversible shutdown and retain
a completion token across cancellation or outstanding-owner errors. Completion
joins all Rostra descendants, closes internally owned transport, and requires
atomic sole ownership before destroying the database. Injected shared services
remain caller-owned. This permits serialized close/reopen without destroying the
host executor; it is neither in-place relocking nor credential zeroization.
Public construction must be awaited rather than cancelled for this lifecycle.

Retained current-state subscriptions expose owned snapshots of the self head,
followees, followers, and Web of Trust. Runtime tasks may keep those snapshots
across network waits or database operations without retaining a Tokio watch
borrow that can block post-commit publication. Lossy broadcasts and
deduplicating work signals retain their incremental semantics.

Verified-data ingestion uses the database's fallible processing boundary.
Invariant, transaction, and storage failures are local database failures, not
peer-availability failures: request and publication paths return them to their
caller, while a background ingestion loop logs the affected identity or event
and stops its affected task or worker. Such failures do not enter peer backoff
or missing-content retry state. The runtime does not globally restart stopped
tasks; reopening or otherwise recovering the client remains an operational
decision.

Temporary payload-capacity pauses are not such failures. Acquisition retains
Missing work, avoids peer backoff, and schedules a bounded later retry so a paused
queue-front event cannot starve other authors. The connection cache owns shared-store
reuse and per-attempt buffer admission; transport APIs retain caller-owned guards
without depending on the database crate. Local publication returns a clear storage
capacity error, while pushed payloads can receive the existing refusal response
before sending bytes. Immutable startup account configuration selects Disabled
(the default), observation-only DryRun, or experimental Enforce.

Payload reads deduplicate the already-authorized candidate accounts. A reachable
author is preferred only when the connection cache already has a live connection;
remaining candidates use exact domain-separated full event/full holder
`RostraId` distance with a full-identity tie-breaker. The bounded read race still
advances to farther candidates after misses or failures. This ordering does not
discover peers, advertise inventory, change trust/backoff eligibility, or use a
device transport key as the retention coordinate. Event-envelope fetching keeps
its existing ordering.

Multi-client hosting can install immutable, explicitly listed account acquisition
ledgers before exposing HTTP or loading a database. The same ledger follows a
verified request into lazy loading, and only one database may attach it at a time.
Manager clones serialize lazy open/build/publication to prevent duplicate opens.
LRU removal retains discoverability of request/task-owned clients and databases:
reload reuses surviving ownership rather than reopening live storage. Retired
database keepers close under the load lock only after atomic sole-ownership
acquisition; a bounded weak-manager reaper releases them after external owners
finish and old tasks terminate. The public builder still consumes its database;
shared-database reconstruction is private to the crate. The cache count therefore
does not bound all live resources. Cold
initialization completes independently of request cancellation so open storage
cannot become undiscoverable before publication.
Unverified unlisted identities do not create registry entries or databases.
Configured retention starts before request handlers and signing tasks, even when
optional replication tasks are disabled. It belongs to the same pre-retained
task group: cancellation, partial-constructor failure and panic all require actual
old-task joins, including tasks without database references, before reconstruction.
DryRun keeps forecast policy separate from the actual Disabled admission ledger.
There is no live reconfiguration API; startup configuration changes require fresh
construction after old client work and guards have quiesced.

Local follow publication serializes mutations per full followee identity and
consults durable active-follow and unfollow-boundary timestamps. It waits for a
strictly later wall-clock second rather than relying on a hash tie-break between
successive local intentions. Queue and clock waits share a bounded admission
budget; future timestamps, exhaustion, and admission timeout return distinct
errors without signing. This is not a total database-I/O latency guarantee.
Publication never invents future logical timestamps or automatically republishes
an event, so follow-epoch notification cutoffs retain their wall-time meaning.
Imports and the replicated total-order reducers are unchanged. This local queue
does not serialize remote same-author ingestion: a remote event arriving after
the ordering snapshot can still supersede the local operation.

Head handling depends on the operation. Local publication and retained state
use the minimum event ID as a deterministic representative. Incremental
broadcasts carry the exact newly accepted head. The broadcaster initially
reconciles the complete durable set, retains current header-first heads until
their content becomes available, and reconciles again after signal lag or
follower-set changes. A newer current descendant subsumes a pending historical
ancestor. Heads in the `Missing` lifecycle state wait for content; explicit
terminal states are discarded. The broadcaster loads at most one ready payload
at a time. Repeated `GET_HEAD` requests independently and uniformly sample the
current set so persistent siblings remain discoverable without changing the v0
wire shape. Paths that require every branch iterate the complete durable set
rather than treating a sample as complete.

`WAIT_HEAD_UPDATE` retains its compatible single-head cursor: it waits while
the caller-provided head remains in the server's current set. It therefore
cannot reveal an already-existing sibling while that known head stays current;
complete immediate fork discovery would require a future bounded full-set or
set-difference RPC. The signing-only head merger scans durable heads immediately
when it starts or the identity is unlocked, then reacts to later changes and
stitches pairs until fewer than two remain.

Full-client activation also starts one owned direct-message worker. It maintains
independent local epoch keys, publishes signed device metadata, and drains the
durable ciphertext queue in bounded, resumable key batches. Startup checks
already-retained work; idle waits register for post-commit notification before
checking the queue and also wake for maintenance. It holds no strong client
reference across idle waits. Client activation is not UI authorization: each
private-message request separately requires its own unlocked session secret.

Head synchronization discovers the graph backward: it must fetch an event's
envelope before it can learn that event's parents. Bounded header-only ancestor
pages let a serving peer follow those links without one client round trip per
generation. The client authenticates the whole page, including the full author
identity and connectivity to the requested root, before consuming bounded
lookahead through the ordinary local-known and cutoff path. Missing or truncated
pages do not establish graph or head-set completeness. Connections that fail the
bounded page probe fall back to single-header requests; payload admission is
unchanged.
The client defers each event's payload, traverses toward older ancestors, and
prioritizes queued payloads by effective timestamp, with deeper ancestors first
when timestamps tie. After discovery, bounded batches overlap payload reads and
ingest ready results without waiting for slower reads of older payloads.
Older-first launch priority is a best-effort UX preference, not a delivery-order
constraint.
Batch count and declared-byte limits apply even when account admission is
disabled; admission guards still cover each racing read through ingestion.
Envelope insertion proceeds from each discovered child toward its parents,
and sibling-envelope discovery follows event ID order rather than strict
depth-first search. Missing data, concurrent branches, peer availability, retries,
and previously stored events can change the observed processing order, so this
is neither a canonical ordering nor a convergence guarantee. Reception order
remains database-local as specified by
[ARCH-client-database](../../rostra-client-db/specs/ARCH-client-database.md).

Publication constructs content events through `rostra-core`, selects the
deterministic representative as its default previous parent, signs with the
unlocked identity key, and stores through the
same database processing path used for received data. This keeps local and
remote validation and projections aligned with
[SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md).
