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

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
