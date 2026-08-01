# rostra-util-dedup-chan

A multi-receiver work channel that suppresses duplicate items while receivers
have them queued. It is useful when repeated state changes should wake workers
without unboundedly repeating the same work.

Rostra uses this crate internally, but it is independently usable when its
queueing semantics fit an application.

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
