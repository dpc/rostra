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

#### Checkout Code, build and run 

Pre-requisite: You should have rust installed on your system, so that you can build the code. 

1. Checkout code using `git clone https://github.com/dpc/rostra`, then do `cd rostra/`
2. Build the binaries using `cargo build`
3. run the web-ui using `./target/build/rostra web-ui`. This will open the web-ui on localhost.

To verify that the above is working well, login to your above localhost web-ui using same 'account or private key' that you use to access rostra.me. Both should show same posts from your account, and from other accounts you follow. 

#### Using Cargo

You can clone the git repository locally and run:

```
cargo run --release web-ui  
```

to start the web ui.

**NOTE**: [`cargo install` can't bundle web UI assets embedded in the git
repository, which are necessary for the web UI to work.](https://github.com/dpc/rostra/discussions/7).


#### Using Nix

You can run Rostra using Nix with:

```
nix run github:dpc/rostra
```

#### Using prebuilt binaries

The [CI builds binaries](https://github.com/dpc/rostra/actions/workflows/ci.yml?query=branch%3Amaster):

* portable Linux x86_64 binary
* DEB package
* RPM package

Pick the last build and at the bottom of the page look for "Artifacts".

In the future, the official releases will come with prebuilt binaries as well.

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

## License

Rostra code is licenses under any of your choosing:

* MPL-2.0
* Apache-2.0
* MIT

The code vendors source code for 3rd party projects:

* [htmx](https://github.com/bigskysoftware/htmx/) - Zero-Clause BSD
* [emoji-picker-element](https://github.com/nolanlawson/emoji-picker-element) - Apache 2.0
* [text-field-edit](https://github.com/fregante/text-field-edit) - MIT
* [mathjax](https://github.com/mathjax/MathJax-src/) - Apache 2.0
