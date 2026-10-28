# rostra-client-db

The embedded state, graph storage, and materialized projections used by a
Rostra client for one local identity. It supports durable and in-memory client
operation and accepts verified Rostra event data.

This crate is intended for Rostra's client runtime and advanced integrations.
Most applications should start with
[`rostra-client`](https://docs.rs/rostra-client), which composes storage,
transport, discovery, and synchronization.

## Bounded local people search

`Database::search_known_people(query, cursor)` searches only self, direct
followees, and their followees. It preserves the web's Unicode lowercase,
subsequence name/full-ID score, full-ID tie break, and ten-result limit for
supported profiles in an unchanged graph. Each call replaces the prior results
with cumulative best-so-far rows; do not append them. Empty pages can continue.
Queries are limited to 256 UTF-8 bytes, graph advances to 128 per call, and
profile observations to eight. Profile inspection borrows at most 2 MiB per
record, copies at most 1024 name bytes, and never copies a bio or avatar.

`Exhausted` means graph traversal ended, not a consistent snapshot or an
unqualified global top ten. `unscorable_profiles` preserves coverage warnings
even for candidates omitted from the result list. Missing profiles use the
ordinary web ID fallback; oversized, corrupt or source-mismatched profiles have
explicit field issues. Names are local projected metadata, not a fresh
verification against signed content; timestamps require a matching retained,
verified source envelope.

`Database::lookup_known_person(full_id, cursor)` separates candidate-graph
membership (`Known`, `Unknown`, `Incomplete`), exact retained-author evidence,
and profile availability. Self/direct follows do not need an author-index entry.
An incomplete scan must not be presented as unknown; unknown never means the
identity does not exist globally. A short-prefix match is not full-ID evidence.

Both cursors are opaque in-process values, bound to account, query/target, open
instance and projection rebuild. Keep at most one per bounded host search
session, discard on reset, and refresh after stale-cursor errors. Traversal is
live: changed names and edges can invalidate rankings, and inserts behind the
cursor require refresh. These APIs perform no network requests or mutations.
Run calls off the UI executor and bound concurrency; byte/step limits are not a
CPU deadline or a process RSS guarantee.

## Native retained media

The opt-in `native-media` feature exposes
`Database::read_native_media(author, MediaEventReference)`. Pass the actual
retained source post author and a typed full/short reference from
`rostra-core::media`. It reads only local signed `SOCIAL_MEDIA`, with no fetch or
mutation. The result distinguishes unavailable lifecycle states, mismatched
identity/kind, unsupported formats, invalid data and policy limits. Available
results include exact author/full event ID, verified MIME/dimensions and original
encoded bytes.

The native subset accepts static PNG/WebP up to 1,000,000 encoded bytes, 2048 per
dimension and 4,000,000 pixels. The separate 2,000,128-byte CBOR body cap supports
the existing integer-array serializer; unbounded deserialization is never used.
Alternative encodings outside this subset are unsupported, not protocol-invalid.
Bytes and metadata are preserved, not normalized. Hosts must explicitly load,
bound concurrent work off the UI executor, and re-read on later presentation
(including offline redisplay). Availability is a snapshot, not an enduring cache
capability. No CPU deadline or process RSS bound is claimed.

For explicit opaque Save/export, the same feature also exposes
`Database::read_native_media_opaque(author, MediaEventReference)`, returning
`DbResult<OpaqueMediaRead>`. Its `Available { author, event_id, mime, data }`
preserves exact original bytes and full identity under the same authentication,
lifecycle, hash, storage and complete CBOR bounds. It omits raster validation:
JPEG, video and arbitrary bytes may be available, without being safe to execute,
open or display. The original `mime: String` is at most 64 UTF-8 bytes of
untrusted metadata, not validated syntax or a filename/extension/dispatch policy.
The 1,000,000-byte data limit and existing CBOR subset still apply; historical
alternative encodings remain `Unsupported`. `Missing`, `Deleted`, `Pruned`,
`Invalid`, `Unsupported`, `TooLarge` and `Mismatch` carry no bytes, while database
operation failures remain outer `DbError`s. The same actual-source-author,
explicit-load, bounded-concurrency and later-use lifecycle recheck obligations
apply. No network, signing, writes or persistent cache are added. Rust clients
can call this through `Client::db()`; bindings should preserve the distinction
between media outcomes and database failures.

`Database::list_own_media` returns bounded pages of own-author retained candidate
metadata without pixel decoding or full image allocation. Each candidate carries
a full event ID and `RequiresReadValidation`, not a safe-image claim. Call
`read_native_media` with the exact author/full ID before preview or reuse.
An empty page can still have a continuation. Cursors are live, exclusive and
bound to the current account/open/rebuild lifetime; do not persist them.

`Database::list_own_public_attachments` separately lists metadata for retained
own-author `application/octet-stream` payloads containing 1..=1,000,000 original
bytes. It verifies signed ownership/full identity, lifecycle, length/hash and the
complete borrowed default-policy CBOR subset without copying payloads. It does
not imply safe-to-open content or relax image listing. Before reuse insertion,
call `read_native_media_opaque` with the owner and exact full ID and recheck
Available, author/ID, exact MIME and nonempty size; do not republish the bytes.
The list charges a full 2,000,128-byte body reservation per raw own-index row,
including other kinds, and stops after eight reservations (16,001,024 body bytes,
plus at most nine wrapper bytes per row). This is tighter than its 128-row and
twenty-item ceilings. `skipped_rows` includes unavailable, unsupported, oversized
and corrupt candidates, not only corruption; database errors still propagate.
Its separate opaque cursor has the same live account/open/rebuild binding.
`BudgetReached` always carries continuation, even for an empty page; only
`Exhausted` ends traversal. Reaching a budget at the last row may require a final
empty call. No uncharged lookahead, global scan, network work or list cache is
introduced.

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
