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

Pkarr maps a Rostra identity to its advertised iroh endpoint and current graph
head. Iroh supplies encrypted peer transport under the Rostra v0 ALPN.
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

Publication constructs content events through `rostra-core`, selects current
graph parents, signs with the unlocked identity key, and stores through the
same database processing path used for received data. This keeps local and
remote validation and projections aligned with
[SPEC-event-graph](../../rostra-core/specs/SPEC-event-graph.md).
