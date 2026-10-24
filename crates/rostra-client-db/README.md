# rostra-client-db

The embedded state, graph storage, and materialized projections used by a
Rostra client for one local identity. It supports durable and in-memory client
operation and accepts verified Rostra event data.

This crate is intended for Rostra's client runtime and advanced integrations.
Most applications should start with
[`rostra-client`](https://docs.rs/rostra-client), which composes storage,
transport, discovery, and synchronization.

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

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
