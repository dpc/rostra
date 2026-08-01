# rostra-p2p

Iroh-based transport and RPC support for the Rostra protocol. It carries
protocol messages and validates received core values at the transport boundary.

This crate is a lower-level implementation layer, not a complete application
runtime. Applications that operate a Rostra node should normally depend on
[`rostra-client`](https://docs.rs/rostra-client).

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
