# ARCH-rostra: Rostra system architecture

Rostra is a friend-to-friend social network whose shared data is a graph of
signed, content-addressed events. Each running client represents one Rostra
identity, maintains the portion of the graph it knows, exchanges graph data
with peers, and derives social views from event content. Full clients persist
this state; light clients use transient in-memory storage.

The event model is specified by
[SPEC-event-graph](../crates/rostra-core/specs/SPEC-event-graph.md). The older
[technical overview](../ARCHITECTURE.md) remains a short conceptual
introduction, while [the non-technical design](../docs/design.md) explains the
friend-to-friend and persona product motivations.

## Components and dependency direction

- `rostra-core` is the dependency root for identities, event headers,
  signatures, content hashes, content kinds, and verified-event types. It does
  not perform storage or networking.
- `rostra-p2p-api` publishes the shared iroh ALPN. `rostra-p2p` defines the
  request and response wire messages, implements typed connections over iroh,
  and validates received core values at the transport boundary.
- `rostra-client-db` owns the graph state and its materialized indexes for one
  identity, using durable or in-memory storage according to client mode. Its
  boundaries and transaction model are described by
  [ARCH-client-database](../crates/rostra-client-db/specs/ARCH-client-database.md).
- `rostra-client` composes identity discovery, p2p transport, the database,
  event publication, replication, and long-running synchronization tasks as
  described by
  [ARCH-client-runtime](../crates/rostra-client/specs/ARCH-client-runtime.md).
- `rostra-web-ui` is the primary presentation and HTTP API layer. It manages
  multiple clients, sessions, and in-memory unlocked credentials, and uses
  client and database APIs rather than owning protocol state. Its HTML-first
  interaction boundary is governed by
  [DESIGN-server-rendered-hypermedia](../crates/rostra-web-ui/specs/DESIGN-server-rendered-hypermedia.md).
  The external API is documented in [docs/web-api.md](../docs/web-api.md).
- The `rostra` binary is the composition root for command-line operation and
  the web UI. Bot and utility crates are consumers or supporting
  infrastructure rather than independent architectural layers.

Dependencies flow from presentation and executables toward the client, then
database/network adapters, and finally core protocol types. The database also
uses p2p types for persisted endpoint information, but networking orchestration
belongs to the client.

## Primary data flow

An author creates content through a client-facing API. The client constructs
and signs an event whose header commits to the content and graph parents, then
stores it through the database. Database transactions update the graph,
content lifecycle, and derived social indexes atomically. Post-commit
notifications wake client tasks and presentation subscribers.

For replication, Pkarr resolves a Rostra identity to current discovery data
and iroh transports Rostra RPCs. Incoming event headers are verified before
database insertion. Missing parents and separately stored content are fetched
asynchronously; successfully processed content updates derived indexes.
Followers and extended followees define the normal synchronization scope.

## System invariants and boundaries

- Rostra identity keys authenticate event authors; transport connections are
  not a substitute for event signature and content verification.
- Event headers and payload bytes remain separable so graph discovery can
  proceed without downloading or retaining every payload.
- A database is scoped to one local identity and is the authoritative state
  for that client's known graph and derived views during its lifetime. A full
  client's disk-backed database preserves that state across runs.
- Secret identity material is required only for active publication. Read-only
  clients can synchronize and serve stored data without it.
- Network input is untrusted until validated by the core verification types
  and database processing path.
- The web UI may hold unlocked credentials in memory for a session, but
  protocol signing remains a client responsibility and social state remains a
  database responsibility.
