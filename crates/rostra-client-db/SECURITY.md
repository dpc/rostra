# Client database security and reliability

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

Schema 25 performs a forward-only total rebuild. Preparation atomically stashes
retained source and installs the current schema; replay and stash cleanup commit
atomically. Any replay error leaves the complete stash retryable, and code must
detect an existing stash before preparing another migration. Historical source
encodings require fixtures using the corresponding released layouts. A
malformed authoritative stash must fail closed; disposable metadata corruption
must not destroy authoritative source.

Replay runs before the database is published, suppresses incremental hooks, and
refreshes current-state watches after commit. It does not retain an event graph
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
