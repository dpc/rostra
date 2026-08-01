# rostra-core

Core types for Rostra's signed, content-addressed event graph. This crate
defines identities, event identifiers, event headers, content commitments, and
verification-related types shared by Rostra implementations.

`rostra-core` does not provide storage, discovery, or peer-to-peer operation.
Applications that need a running Rostra node should start with
[`rostra-client`](https://docs.rs/rostra-client).

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
