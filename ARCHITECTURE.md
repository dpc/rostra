## Rostra: Technical Architecture

## Lightewight event DAGs

In Rostra all data is propagated in forms of `Event`s.
The events are a tiny fixed-sized packet, that
is being signed over by the author. Cryptographic hash of
the [`Event`] is an `EventId` and uniquely identifies the event.

Among some other minor things, `Event` includes `EventId`s of
up to two previous `Event`s

Thanks to this the history of all past events of the given user
forms a dag, that can be traversered (and replicated) from the newer
to the later events. Among other benefits, this allows seamless
multi-device use - any disjoint parts of the history can get
"stitched together" with any newer event pointing to both.

The [`Event`] also includes `ContentHash` - a cryptographic hash
of the actually payload carried.

This defers data synchronization, and allows selective (incremental, partial, etc)
data synchronization.

## Network architecture

Rostra utilizes [Pkarr][pkarr] as a sovereign distributed naming/identy system,
and [iroh-net][iroh-net] as a p2p transport layer.

[pkarr]: https://github.com/pubky/pkarr
[iroh-net]: https://github.com/n0-computer/iroh

Any Rostra node can connect with any other node, and
identity's connectivity information and latest state
can be bootstrapped using Pkarr, after which nodes
can communicate using Iroh's built-in discovery mechanism.


Rostra node can be "full" (download and store data) or "light" (no persistence),
potentially with a variety of storage policies.

A single user/identity can run multiple nodes for a combination of:

* privacy
* availability
* multi-device use


Events and their content are synchronized in real-time,
based on the social graph information.


## Points of Interest

To understand the inner workings in more details, here are some POIs (might go stale over time):

* `struct Event` - https://github.com/search?q=repo%3Adpc%2Frostra+struct+Event&type=code
