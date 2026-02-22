# Rostra

Rostra is a p2p (or rather f2f) social network.

## Overview

In Rostra users "follow" (subscribe) to other identities like in
many conventional social network systems. Followers track, download
and help share data of users they follow.

Rostra is a mostly-f2f ("friend to friend"), meaning
there is no "global feed". Users can discover new identities
and content only through existing connections.

Rostra comes with support for "personas" (sub-identities), to allow
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

## Introduction to Rostra

[<img src="https://img.youtube.com/vi/KjUVKUOg4aE/maxresdefault.jpg" alt="Introduction to Rostra" />](https://www.youtube.com/watch?v=KjUVKUOg4aE)

*[▶ Watch on YouTube](https://www.youtube.com/watch?v=KjUVKUOg4aE)*

## Using

### Using a public instance

A public instance is available at <https://rostra.me/>.

Anyone can host their own public or private instance, or run Rostra locally.


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
(it rarely actually is) to enable making direct p2p connections with other users.

You can host Rostra on your server, and use it remotely over the web,
the same way <https://rostra.me> is working.


#### Running over Tor

For best privacy/anonymity it is possible to run Rostra over Tor using
[`oniux`](https://blog.torproject.org/introducing-oniux-tor-isolation-using-linux-namespaces/)

You can run:

```
nix run github:/dpc/rostra#rostra-web-ui-tor
```

to use a script to do so. See [`flake.nix`](./flake.nix) to investigate how it works.


## More info about Rostra

* [Architecture overview](./ARCHITECTURE.md)
* [Design decisions](./docs/design.md)
* [FAQ](./docs/FAQ.md)
* [Comparison with other social protocols](./docs/comparison.md)
* [`HACKING.md`](./HACKING.md)
* [Github Discussions](https://github.com/dpc/rostra/discussions)


## License

Rostra code is licensed under any of your choosing:

* MPL-2.0
* Apache-2.0
* MIT

The code vendors source code for 3rd party projects:

* [Alpine.js](https://alpinejs.dev/) 3.14.3 - MIT - UI interactivity
  * [Persist plugin](https://alpinejs.dev/plugins/persist) - MIT - UI state persistence
  * [Intersect plugin](https://alpinejs.dev/plugins/intersect) - MIT - infinite scroll
  * [Morph plugin](https://alpinejs.dev/plugins/morph) - MIT - DOM diffing for WebSocket updates
* [alpine-ajax](https://alpine-ajax.js.org/) 0.12.6 - MIT - declarative AJAX
* [emoji-picker-element](https://github.com/nolanlawson/emoji-picker-element) - Apache 2.0 - emoji picker
* [text-field-edit](https://github.com/fregante/text-field-edit) - MIT - textarea cursor manipulation
* [MathJax](https://github.com/mathjax/MathJax-src/) 3.2.2 - Apache 2.0 - LaTeX rendering in posts
* [Prism.js](https://prismjs.com/) - MIT - syntax highlighting in code blocks


## AI usage disclosure

[✨ I use LLMs when working on my projects. ✨](https://dpc.pw/posts/personal-ai-usage-disclosure/)
