## Rostra: Non-technical design

For technical design, please see [../ARCHITECTURE.md].

### F2F

In my mind the biggest problem with online social networks is the abuse protection.
Any form of "global view" bring incentive to spam, haras, exploit. That's why
I think practical p2p social network has to be f2f ("friend to friend").
Any user should only see content
from people they decided to follow, which might include content from people they follow.
Don't want to see nude pictures? Don't follow users posting nude pictures, or who
follow people posting nude pictures. Don't like certain radical political opinions,
don't follow people who voice them. Thanks to this no central policing is necessary.

In all forms of "discoverability" (viewing content indirection through one of the users
we follow), it should be always easy to attribute which user is it.

This also naturally falls into storage and bandwidth requirements. User's content
will be naturally replicated proportionally to how many other users care about it.
No need for central servers, indexers, etc.


### "Personas"

On every social network ever, I have this problem that I post 75% dev stuff, yet
as a real person I want sometimes post a music video I like, or a silly joke,
or a reaction to an event, and I realize that many people who follow me for technical
content might not care.

On the receiving end, I sometimes need to endure someones silly politics
because I like their tech work.

Other social media platform encourage publicity through focus on a single issue
and mob-gathering.

That's why I think every social network that want people to be "wholesome"
and not self-censor to optimize target audience needs to implement posting/following
to/from sub-identities with a simple and convenient UX: Follow my tech work, ignore
my personal opinions, or in reverse, or both. You choose.
