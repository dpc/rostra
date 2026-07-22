# SPEC-event-graph: Signed event graph

## Status

The shortened-identity collision guard applies to new event ingestion and total
replay. Existing version-24 client databases are not proactively scanned, so a
mapping written before the guard may remain until it is encountered. The final
total rebuild tracked by `t4vh` will validate retained event authors under the
guard.

## Record justification

Graph semantics span core event encoding and verification, client database
storage and lifecycle transitions, replication, and application content
processing. No single implementation area can document the complete contract.

Rostra data is represented by compact signed event headers and separately
addressed content. This model is the shared contract used by storage,
replication, and application content processing. It refines the system shape
in [ARCH-rostra](../../../specs/ARCH-rostra.md).

## Identity and integrity

An event names its author with a `RostraId`. Its signature must verify against
that identity, and its event ID is derived from the signed event header.
Callers receiving an event in response to a request must also verify that the
computed identity and event ID match what was requested.
Any separate author claim in a protocol response remains untrusted until it
matches the author authenticated by the event signature. Synchronization scope
or Web-of-Trust admission must use that bound, authenticated identity rather
than independently trusting response metadata.

An event header commits to a content hash and content length. Payload bytes are
valid for that event only when both values match. Code that accepts
`VerifiedEvent` or `VerifiedEventContent` may rely on these checks having
already succeeded; unverified wire or decoded values must cross a verification
boundary first.

Local storage may abbreviate a `RostraId` by its 128-bit prefix only while
retaining a one-to-one mapping to the remaining 128 bits. Reusing a prefix for
the same full identity is idempotent. If two distinct full identities share a
prefix, ingestion fails without replacing the established, first-committed
mapping or committing graph, lifecycle, or projection changes. During total
migration, the replay transaction fails and rolls back while the separately
prepared source stash remains available for a deterministic retry.

## Graph structure

Each event can name a previous parent and an auxiliary parent. Both parent IDs
are relative to the envelope author and can resolve only to events signed by
that same author. An event by another author with the same raw parent ID is not
the named parent. Missing parents are valid, including for the first event,
light clients, and data received out of order. Storage therefore keeps such a
cross-author raw-ID match unresolved for the envelope author, just as when no
matching row exists. The two parents may be equal. Together, events from one
author form a graph that can merge concurrently produced branches and can be
traversed from newer events toward older history.

The previous parent normally identifies a current event known to the author.
The auxiliary parent may merge another branch, carry kind-specific meaning, or
point farther into history to accelerate traversal. Storage and replication
must tolerate unknown parents and fetch them later rather than rejecting an
otherwise valid event.

For one author, the current **head set** contains every accepted event with no
known same-author child. Concurrent writers and out-of-order replication can
make this set contain multiple members; no member is intrinsically canonical,
newest, or preferred. A singleton-returning interface therefore does not prove
that only one head exists. Local append may use a deterministic representative
as its default previous parent, incremental propagation should identify the
event that caused the signal, repeated one-head discovery may sample the set,
and synchronization that requires every branch must use explicit complete-set
or set-difference semantics.

## Content semantics

`EventKind` identifies how payload bytes are interpreted. Keeping the kind,
hash, and length in the header permits selection and size policy before
payload retrieval. Identical payloads may be shared by multiple events because
content is addressed by hash.

The delete-parent-content flag declares that the content of the same-author
auxiliary parent is deleted; the graph header remains available. It must never
delete or otherwise mutate an event signed by another author whose raw event
ID matches the auxiliary parent field. The singleton flag declares that only
the latest event for the same kind and auxiliary key matters. "Latest" is the
maximum `(event.timestamp, ShortEventId)`, so distinct events in the same
timestamp second have one canonical winner. Consumers must use this complete
order for singleton-derived state and preserve graph-level semantics even when
the affected payload is absent locally.

Deletion affects only the target payload. It does not disable the target header
or any parent or deletion instruction encoded by that header. Deleting a
deletion event's payload therefore neither cancels that event's deletion of its
direct auxiliary parent nor transitively changes the earlier target's deleting
event.

Deletion is signed, after-the-fact metadata for compliant clients, not a secure
erasure primitive. It controls payload visibility and lifecycle handling on
replicas that honor it, but the protocol cannot force remote or malicious
replicas to erase bytes they already received. This best-effort limitation is
fundamental to replicated peer-to-peer storage.

Unknown flag bits are not produced by the current implementation, but readers
accept and ignore bits they do not understand so later protocol versions can
assign them. Current producers emit event version zero. Assigning semantics to
a newer version requires explicit protocol support rather than assuming
version-zero semantics.

The database realization of separation, deletion, pruning, and out-of-order
content is specified by
[SPEC-event-content-lifecycle](../../rostra-client-db/specs/SPEC-event-content-lifecycle.md).
