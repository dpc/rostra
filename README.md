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

## Plan

Status: early, but getting to something usable.

* Figure out general idea and architecture (mostly done)
* Implement data model, p2p rpc, replication, etc.
* Implement a built-in UX (web-server, using htmx).

## More info about Rostra:

* [Architecture overview](./ARCHITECTURE.md)
* [Design decision](./docs/design.md)
* [FAQ](/docs/FAQ.md)
* [Rostra: comparison with other social media projects](/docs/comparison.md)
* [`HACKING.md`](./HACKING.md)
