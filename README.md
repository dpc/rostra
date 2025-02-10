# Rostra

Rostra is a p2p (or rather f2f) social network.

## Overview

Rostra is a p2p, censorship-resistant social platform.
There are no centralized user accounts, only self-generated,
sovereign identities.

In Rostra users "follow" (subscribe) to other identities like in
many conventional social network systems. Followers track, download
and help share data of users they follow.

Rostra is a mostly-f2f ("friend to friend"), meaning
there is no "global feed". Users can discover new identities
and content only through existing connections.

Rostra comes with a support for "personas" (sub-identities), to allow
publishing/following to/from a subset of person's identity.
E.g. you might be interested in someone's technical work,
but not their political opinions. By supporting sub-identities,
users can remain wholesome and post without risking diluting
their most popular aspects and/or alienating their followers.

Rostra is extensible, and can be used for applications other
than Twitter-like-app, as long as it fits its general
social-graph-based data model.

## Using

The discoverability story is bleak ATM, so you
probably want to follow me: `rstr1gjs29qd6qfam47y45ph87tdtpxzce9awy77cfjzfvgws4xfcpgxq6rc4ry`
if you don't want to stare at an empty timeline.

#### Demo public instance

A public instance is available at https://rostra.me/ , but
it's advised to run Rostra directly on your system.

#### Using Cargo

As a Rust project Rostra can be installed using Cargo:

```
cargo install --git https://github.com/dpc/rostra
```

#### Using Nix

You can run Rostra using Nix with:

```
nix run github:dpc/rostra
```

## Plan

Status: early, but getting to something usable.

* Figure out general idea and architecture (mostly done)
* Implement data model, p2p rpc, replication, etc.
* Implement a built-in UI (web-server, using htmx).

## More info about Rostra:

* [Architecture overview](./ARCHITECTURE.md)
* [Design decisions](./docs/design.md)
* [FAQ](/docs/FAQ.md)
* [Comparison with other social protocols](/docs/comparison.md)
* [`HACKING.md`](./HACKING.md)
