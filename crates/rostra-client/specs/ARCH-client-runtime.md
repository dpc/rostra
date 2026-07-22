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
temporary peer failure, and out-of-order delivery. Task handles are owned by
the client, so dropping the client stops its background work.

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

Head synchronization discovers the graph backward: it must fetch a newer
event's envelope before it can learn that event's parents. It then defers that
event's payload, traverses backward toward older ancestors, and prioritizes
queued payloads by effective timestamp, with deeper ancestors first when
timestamps tie. This older-first, depth-biased traversal deliberately makes
payload processing order opportunistically approximate author production order.
Envelope insertion still proceeds from each discovered child toward its parents,
and sibling-envelope discovery is ordered by event ID rather than strict
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
