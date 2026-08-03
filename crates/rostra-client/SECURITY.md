# Client security and reliability

`rostra-client` is the asynchronous runtime boundary for one Rostra identity.
Full clients run background synchronization tasks and persist accepted graph
state; light clients use an in-memory database. Peers can influence every RPC
response, event envelope, payload, endpoint announcement, and synchronization
wakeup. The runtime shape and task ownership are described by
[ARCH-client-runtime](specs/ARCH-client-runtime.md), while signed-event
integrity and graph invariants are specified by
[SPEC-event-graph](../rostra-core/specs/SPEC-event-graph.md).

Production ingestion uses the database's fallible APIs. Transaction, invariant,
or storage failure rolls back the affected transaction and returns from a
request or publication boundary, or logs the affected identity/event and stops
the background task or worker performing that ingestion. Such failures never
enter peer backoff or missing-content retry state; ordinary network absence
keeps its existing bounded retry or backoff behavior. Stopped tasks are not
globally restarted, so reopening or otherwise recovering the client is an
operational decision. The database's panic-on-error compatibility wrappers are
preserved for existing callers and tests but are not production ingestion APIs.
`failed_announcement_does_not_commit_activation_and_retry_can_start_tasks` and
`storage_failure_stops_before_resetting_peer_backoff` protect the activation and
peer-failure classification boundaries.

An encrypted iroh connection authenticates the remote transport endpoint, not
the Rostra author of an event and not that author's admission to the local Web
of Trust. Event signatures authenticate Rostra authors. Unsigned routing fields,
claimed identities, event IDs, and other response metadata remain
peer-controlled until explicitly bound to verified signed data.

For `WAIT_FOLLOWERS_NEW_HEADS`, consumers must enforce this order:

1. require the response's claimed author to equal the event author and verify
   the event signature;
2. check the authenticated event author against the current local Web of Trust;
3. only then insert the event envelope into the database.

An author mismatch, invalid signature, or other verification failure rejects
the poll before database mutation and enters the peer's ordinary failure and
backoff path. A valid event from an authenticated author outside the current
Web of Trust is ignored without insertion. Payload retrieval remains deferred
to the bounded background synchronization path.

The
`rpc_rejects_trusted_claim_with_event_signed_by_another_author` regression
protects the reject-before-storage boundary, and
`rpc_ingests_event_when_claimed_and_signed_authors_match` protects normal RPC
ingestion. Both exercise the typed RPC over an in-memory iroh connection rather
than calling response validation in isolation.

Inbound serving admits at most 128 simultaneous connections and 256 simultaneous
RPC handlers per client, while retaining the 32-handler per-connection limit.
Long polls may use only 192 shared slots; 64 slots remain reserved for finite
RPCs on already-admitted connections, preventing long polls from consuming
every RPC permit. The connection budget itself has no reserved class: 128
incumbent long-poll connections can deny a new connection indefinitely. The
limits size a client for hundreds of persistent subscribers while keeping a
fixed memory envelope; clients above that subscriber ceiling receive immediate
rejection and must reconnect. The limits do not provide per-identity fairness,
and multiple remote endpoint identities can occupy the shared capacity.

Excess connections and RPC streams are rejected immediately rather than queued.
Production-created endpoints allow 32 remote bidirectional streams per
connection, disable remote unidirectional streams, and use 64 KiB per-stream and
2 MiB per-connection receive windows. A caller-supplied endpoint is a trusted
configuration boundary and must set safe stream/window transport parameters
before binding; post-handshake stream-count reductions cannot retract credit
already advertised by that endpoint. A connection handshake and a bounded
request header each have ten seconds to complete; a connection with no active
RPC closes after two minutes. Every finite RPC, including request-body upload
and response write, must complete within 60 seconds. `WAIT_HEAD_UPDATE` and
`WAIT_FOLLOWERS_NEW_HEADS` are the only deadline-free RPCs because waiting is
their protocol purpose. Their client-wide and per-connection permits cover the
complete handler lifetime and release on completion or task/connection
cancellation.
`client_wide_admission_stays_bounded_and_recovers_after_release` saturates both
admission primitives, protects the long-poll/finite partition, and proves permit
recovery. `stalled_finite_rpc_times_out_and_connection_recovers` exercises the
real request handler and QUIC stream through a stalled body and a subsequent
ping. `admitted_long_poll_survives_finite_deadline_and_allows_ordinary_rpc`
protects the deadline whitelist and same-connection finite-RPC availability.

Successful follower polls only repoll immediately after inserting a new event.
A valid response that does not change local event state, including an
already-present event or an event outside the local Web of Trust, waits one
second before the next `WAIT_FOLLOWERS_NEW_HEADS` request. This bounds replayed
successful responses without delaying genuinely new head ingestion.
`replayed_follower_head_responses_are_rate_limited` exercises that safeguard
through typed RPC and asserts the complete configured delay using elapsed time.

Followee `WAIT_HEAD_UPDATE` cursor and pending-envelope retry state belongs to
one winning follow-event generation. Slot cancellation preserves both within
that generation, including cancellation between envelope retrieval and durable
ingestion. Unfollow or a new winning follow event cancels the retired poll and
discards its state before scheduling fresh work. This intentionally also resets
state for an update within an uninterrupted follow epoch because generation
identity uses the winning follow event. The
`poll_slots_retain_cursor_and_retry_pending_event_after_cancellation` and
`coalesced_unfollow_readd_cancels_active_epoch_and_prunes_stale_state`
regressions enforce these boundaries.

The v0 wire shape retains a separate author claim for compatibility; security
comes from binding it at every consumer, not from trusting the field. Re-check
this boundary and extend adversarial coverage whenever adding a consumer,
changing a response shape, moving Web-of-Trust admission, or introducing a new
RPC that carries both identity metadata and signed data.
