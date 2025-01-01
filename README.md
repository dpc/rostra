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
without synchronization issues as diverging histories can simply be merged.

To understand the inner workings in more details, here are some POIs (might go stale over time):

* `struct Event` - https://github.com/search?q=repo%3Adpc%2Frostra+struct+Event&type=code
* `fn insert_event_tx` - https://github.com/search?q=repo%3Adpc%2Frostra+insert_event_tx&type=code

## Plan

* Figure out general idea and architecture (mostly done)
* Implement data model, p2p rpc, replication, etc.
* Implement a built-in UX (web-server, using htmx).

## Compared with

Let's be honest. The single biggest reason Rostra exist is: I didn't design any of the alternatives,
so they work differently to how I think it p2p social network should work. :D

### Nostr

Rostra largely draws inspiration from Nostr, and in my mind is "Nostr done right".

Similarities:

* Conceptually simple. That's my favourite part of Nostr.
* Name. I swear I asked Claude.ia to come up with Roman terms related to public discourse first,
  and only then turned out there's one that is oddly similar, so I ran with it for LOLs.

Differences:

* The lower layers of the implementation are more comlicated. It's 2025, I can
  take p2p connectivity, distributed DNS, binary serialization as dependencies, rip
  the benefits with minimal effort and focus on the simplicity of the relevant design.
* Actually P2P. DHTs and P2P work just fine in BitTorrent for 2 decades already.
* Abandons JS, embraces Rust and Unix tech. Optimizing for JS-integration is not
  the most important goal, especially as it sacorifices actually being P2P.
* Optimizes for performanced and resource usage.


### BlueSky

Differences:

* Simpler. One person design.
* Doesn't bring centralized tech like DNS in.
* Self-hosted-first.

## About the design decision

### Why F2F

In my mind the biggest problem with online social networks is the abuse protection.
Any form of "global view" bring incentive to spam, haras, abuse. That's why
I think practical p2p social network has to be f2f. Any user should only see content
from people they decided to follow, which might include content from people they follow.
Don't want to see nude pictures? Don't follow users posting nude pictures, or who
follow people posting nude pictures. Don't like certain radical political opinions,
don't follow people who voice them. Thanks to this no central policing is necessary.

In all forms of "discoverability" (viewing content indirection through one of the users
we follow), it should be always easy to attribute which user is it.

This also naturally falls into storage and bandwidth requirements. User's content
will be naturally replicated proportionally to how many other users care about it.
No need for central servers, indexers, etc.


### Why sub-personas

On every social network ever, I have this problem that I post 75% dev stuff, yet
as a real person I want sometimes post a music video I like, or a silly joke,
or a reaction to an event, and I realize that many people who follow me for technical
content might not care.

On the receiving end, I sometimes need to endure someones idiotic political opinions
because I like their tech work.

That's why I think every social network that want people to be "wholesome"
and not self-censor to optimize target audience needs to implement posting/following
to/from sub-identities with a simple and convenient UX.
