# Rostra

Rostra is a p2p (or rather f2f) social network.

## Overview

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

## Screenshot

![Screenshot of Rostra's p2p social network web UI](https://i.imgur.com/7hGZrP4.png)

## Using

The discoverability story is bleak ATM, so you
probably want to follow me: `rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy`
if you don't want to stare at an empty timeline.

### Public demo

A public instance is available at https://rostra.me/ , but
it's advised to run Rostra directly on your system.

Click "Logout" and then "Random" to generate your
own identity to play with if you want.

You can follow the Rostra developer: `rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy`,
if you want to see more than your own posts. Give it a bit of time to sync with the network

#### Using Cargo

You can clone the git repository locally and run:

```
cargo run --bin rostra --release -- web-ui
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

### Privacy

By default Rostra clients use a relay-only mode, which prevents your IP
from being directly exposed to other peers.

Use `--public` command line argument when exposing IP is not an issue
(it rarely is) to enable making direct p2p connections which can
by much faster.

#### Running over Tor

For best privacy/anonimity it is possible to run Rostra over Tor using
[`oniux`](https://blog.torproject.org/introducing-oniux-tor-isolation-using-linux-namespaces/)

You can run:

```
nix run github:/dpc/rostra#rostra-web-ui-tor
```

to use a script to do so. See [`flake.nix`] to investigate how it works.

Alternatively, you can host Rostra on your server, and use it remotely over web-ui,
in the same way <https://rostra.me> is working.

## More info about Rostra:

* [Architecture overview](./ARCHITECTURE.md)
* [Design decisions](./docs/design.md)
* [FAQ](/docs/FAQ.md)
* [Comparison with other social protocols](/docs/comparison.md)
* [`HACKING.md`](./HACKING.md)
* [Github Discussions](https://github.com/dpc/rostra/discussions)

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

## AI usage disclosure

[✨ I use LLMs when working on my projects. ✨](https://dpc.pw/posts/personal-ai-usage-disclosure/)
