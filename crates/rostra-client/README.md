# rostra-client

The integration entry point for applications that operate a Rostra
peer-to-peer client. It composes Rostra identity discovery, Iroh transport,
local graph storage, event publication, replication, and background
synchronization.

Rostra is a friend-to-friend social network built from signed,
content-addressed events. Use this crate rather than lower-level Rostra crates
unless an integration specifically needs to own a protocol or storage boundary.
The generated API documentation is the authoritative reference for the current
client API.

Source and issue tracking: <https://github.com/dpc/rostra>

## Single-account host lifecycle

For serialized mobile/JNI sessions, finish construction before exposing the
session. To close, stop accepting commands, finish or cancel-and-join application
jobs, and release their client/database/connection owners:

```rust,ignore
let mut closing = client.begin_shutdown()?;
closing.join().await?;
// Only now reopen the database, using the same still-running Tokio runtime.
```

Retain `closing` when a join is cancelled or times out. Retry its `join` rather
than beginning another close. Outstanding client or database owners produce
retryable errors; release them first. A successful join is idempotent. Dropping
the token, dropping the runtime, and OS suspension/kill are not verified close.
There is no guaranteed close deadline and no publication-delivery guarantee.
The API is for directly constructed clients, not manager-owned `MultiClient`
instances.

Internally created iroh endpoints are closed. Injected endpoints and Pkarr
services remain caller-owned; shutdown releases Rostra's handles without
globally stopping a shared service. External transport handles and upstream
internal cleanup are outside the completion guarantee.

Backgrounding or hiding UI alone leaves Rust work and signing active. A host
that closes on background must keep the runtime alive through join. There is no
in-place relock: close and reconstruct read-only, then explicitly unlock when
appropriate. Read-only clients retain no signing authority, but trusted code
can still supply a secret to publication methods.

Shutdown does not erase credentials in caller/managed/native memory, persistent
identity/device keys, stored plaintext, or OS caches, and cannot retract issued
signatures. Clear UI credentials separately. Await public construction rather
than cancelling it: this API does not certify cleanup after a cancelled build.

## Native static-image publication

Enable `native-media` for `Client::publish_media_idempotent`. Normalize selections
with `rostra_core::static_media::normalize_static_media` off the UI executor, then
durably persist the exact PNG bytes, MIME, policy version and a fresh random
`MediaRequestId` before publication. Never normalize pending retries again.
Permanent local receipts return the original full event ID even after deletion
or pruning; success is local acceptance, not delivery. See the API documentation
for shutdown, uncertain errors and permanent receipt privacy.

Normalization rejects unsupported orientation/color metadata rather than
silently changing visuals. ICC/EXIF (including benign defaults) and WebP XMP
are unsupported; canonical sRGB PNG declarations are accepted. Limits are
1,000,000 input/output bytes, 2048 per axis, 4,000,000 pixels, static PNG/WebP only.
The existing historical image reader and web media policy are unchanged.

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
