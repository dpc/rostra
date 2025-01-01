# Rostra

Status: early WIP (nothing for the end user to play with yet)

Rostra is a p2p (or rather f2f) social network.

## Overview

Rostra is p2p and censorship-resistant. There is no user accounts,
only self-generated, suvereign identities.

In Rostra users "follow" (subscribe) to other identities like in
many conventional social network systems. Followers track, download
and help share data of users they follow.

Rostra is a mostly-f2f (friend to friend), meaning
there is no "global feed". Users can discover new identities
and content only through existing connections.

Rostra comes with a support for personal sub-identities, to allow
publishing/following to/from a subset of person's identity.
E.g. you might be interested in someone's technical work,
but not their political opinions. By supporting sub-identities,
users can remain wholesome without risking alienating their
followers.

Rostra is extensible, and can be used for applications other
than social networks, as long as it fits its general data
model.

## Architecture

Rostra utilizes [Pkarr][pkarr] as a suvereign distributed naming/identy system,
and [iroh-net][iroh-net] as a p2p transport layer.

[pkarr]: https://github.com/pubky/pkarr
[iroh-net]: https://github.com/n0-computer/iroh

When a Rostra node starts, it generates a random Iroh endpoint,
and publishes connection details in a Pkarr address. Multiple
devices can use the same identity and use Pkarr to coordinate
to achieve fallback, redundancy and storage offloading. Rostra
node that has most recently published it's Pkarr record is
effectively "active" and will have other nodes connect to it.


Rostra node can be "full" (download and store data) or "light" (no persistence). A "light" node is useful for basic
functionality e.g. publishing new data. This can be useful e.g. to send notifications, alerts, etc. A "light" node will attempt to
connect to an "active" node and forward the data it wants to publish. When not possible, it will register itself as a
temporary "active" node and return new data to any followers asking for it, and "yield" if any "full" node arrives.

In Rostra all data is published in form of Events. An Event
is a short self-signed header committing to actual data with a blake3 hash.
Separating header and data allows quick and reliable history replication,
while supporting potentially large payloads, replicated using bao verified
streams. It also supports volountary data deletion, based on followers
respecting the request to no longer store, circulate and display past content
marked as "deleted" in the new events.

An Event also contains up to two hashes to existing events, forming a mostly-chain-like
DAG, that allows interested users/nodes to track and replicate whole or part of the history.
The DAG is also a way to allow multiple nodes to share the same identity
without synchronization issues as divering histories can be simply merged.

To understand the inner workings in more details, here are some POIs (might go stale over time):

* `struct Event` - https://github.com/search?q=repo%3Adpc%2Frostra+struct+Event&type=code
* `fn insert_event_tx` - https://github.com/search?q=repo%3Adpc%2Frostra+insert_event_tx&type=code

## Plan

* Figure out general idea and architecture (mostly done)
* Implement data model, p2p rpc, replication, etc.
* Implement a built-in UX (web-server, using htmx).
