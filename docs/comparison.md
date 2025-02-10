# Rostra: Comparison with other social media projects

Let's be honest. The single biggest reason Rostra exist is: I didn't
design any of the alternatives, so they work differently to how I think
p2p social network should work. :D

I have no illusions that Rostra has a high chance of gaining traction,
but I always had a lot of opinions about the existing solutions, and
what's more fun than making your own social network, and then being
the only user on it, right? At least the posts are always bangers.


But let's examine differences between similar projects I'm aware of.

### vs everything else

Differences:

* Concept of "Personas" to allow "wholesomeness".
* Blazingly fast.
* Cool tech stack.
* Max P2P.

### vs Twitter

I don't get enough publicity on Twitter, and it must be the algorithm
suppressing me. Obviously.

Plus Ads. Plus centralized. The free speech is kind of a mixed bag.

Differences:

* No ads. No algorithms.
* Rostra is P2P.
* No one can stop you from using it.
* You can host it on your server, you can run it from your desktop, or
  any combination of devices at the same time.
* No users. :D (but it's fixable!)

### vs Mastodon

Mastodon is Federated, not P2P. If I'm getting off the Twitter, I'm not
switching to a solution where I can get my account banned from major public
instances, or my self-hosted instance banned from parts the Federation.

Can't really run it from my local machine either.

Differences:

* You own your identity.
* No one can stop you from publishing (though no one is forced to follow you
  either).

### vs Secure Scuttlebutt

I enjoyed the idea behind SSB a while ago, but it went largely nowhere for years, and I don't
understand why. Admitely, I stopped caring about it years ago, so maybe something
changed in the meantime?

It isn't entirely p2p, as it uses "pubs" to exchange messages. OK-ish, but
in Rostra every node can connect directly, and every node acts as a "pub".

Every time I looked into details of its design, I was disappointed.

It couldn't solve the multi-device/account problem. Too much JS, if you ask me. :D.
I can't help them because I am not touching JavaScript.


Differences:

* No pubs. Actually p2p.
* Less JS.
* DAG of events instead of a a chain of events. This solves all the SSB problems (see ./design.md).

### vs Nostr

Big part of my motivation to work on Rostra is how meh Nostr is. :D

Nostr basically re-discovers SSB, because why learn from the past, when
you can simply ignore it.

Comparing to SSB "pub"s are now called "relays" and do slightly more work,
which is OK when you have no users anyway. The design is roughly as clunky as SSB was,
but optimized for a web-developer, so anyone can easily build another half-assed
client, demo it and feel good about themselves.

Compared to SSB doesn't have a concept of a chain of events, so you never know
if you actually received all the past messages.

Got mildly popular because Bitcoin community was socially adjacent and ran with it.
The Ostrich is kind of cool, and the meme game is A+, so yeah. NGL, I'm a bit envious and salty,
if you can't tell. :D


Similarities:

* Name. I swear I asked Claude.ai to come up with Roman terms related to public discourse first,
  and only then turned out there's one that is oddly similar, so I ran with it for LOLs.

Differences:

* No relays. Actually p2p.
* Scales. (I hope, LOL)
* The lower layers of the implementation are more advanced tech-wise, for a good reason.
  Can still expose simple "sign me a json" API-s too.
* Abandons JS, embraces Rust and Unix tech. Optimizing for JS-integration is non-goal,
  especially if it was to sacrifice being P2P.
* Optimizes for performance and resource usage.
* Events are chained together in a DAG.


### vs BlueSky

Differences:

* Simpler. One person design.
* Self-hosted-first.
* No dead butterflies.
