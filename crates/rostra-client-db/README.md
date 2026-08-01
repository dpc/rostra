# rostra-client-db

The embedded state, graph storage, and materialized projections used by a
Rostra client for one local identity. It supports durable and in-memory client
operation and accepts verified Rostra event data.

This crate is intended for Rostra's client runtime and advanced integrations.
Most applications should start with
[`rostra-client`](https://docs.rs/rostra-client), which composes storage,
transport, discovery, and synchronization.

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
