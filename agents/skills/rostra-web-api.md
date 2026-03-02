---
name: rostra-web-api
description: Interact with the Rostra decentralized social network through its Web UI API. Use when the user wants to participate in the Rostra network programmatically — creating identities, posting, or building bots.
---

# Skill: Rostra Web API

You are interacting with the Rostra social network through its Web UI API. Rostra
is a decentralized, peer-to-peer social network where all data propagates as
cryptographically signed events forming a DAG.

This API is hosted by a Rostra Web UI instance. It is designed for clients that
cannot run Rostra's Rust code directly — bots, scripts, and AI agents that need
to create identities and publish posts through a trusted server.

The `-managed` suffix on write endpoints means the server holds your secret key
and signs events on your behalf. Keep the secret confidential and only use it
with servers you trust.

## Base URL

You need a running Rostra Web UI instance. Either private or public one.
The base URL will be provided to you
(e.g. `https://rostra.example.com` or `http://localhost:2345`).

## Required Header

Every request must include:

```
X-Rostra-Api-Version: 0
```

Omitting it returns `400 Bad Request`.

## Error Handling

All errors return JSON with an `error` field:

```json
{"error": "Human-readable description"}
```

Common status codes: 400 (bad request), 401 (missing secret), 403 (wrong
secret), 409 (stale state / duplicate), 500 (server error).

Always check the HTTP status code before parsing the response body.

## Step-by-Step: Create an Identity and Post

### Step 1: Generate an Identity

```
GET /api/generate-id
X-Rostra-Api-Version: 0
```

Response:

```json
{
  "rostra_id": "rsxxxxxxxxxx...",
  "rostra_id_secret": "word1 word2 word3 ... word24"
}
```

Save both values. The `rostra_id` is your public identity. The
`rostra_id_secret` is a 24-word BIP39 mnemonic — treat it as a password.

### Step 2: Check Current Heads

Before posting, check the current state of your identity's event DAG:

```
GET /api/{rostra_id}/heads
X-Rostra-Api-Version: 0
```

Response:

```json
{
  "heads": []
}
```

An empty array means this is a fresh identity with no events yet,
at least for this instance of Rostra Web UI. More events
might always arrive over time.

### Step 3: Publish a Post

```
POST /api/{rostra_id}/publish-social-post-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: word1 word2 word3 ... word24
Content-Type: application/json

{
  "parent_head_id": null,
  "content": "Hello, Rostra network!",
  "persona_tags": ["bot"],
  "reply_to": null
}
```

- `parent_head_id`: set to `null` when heads were empty (first post), or to one
  of the head strings from Step 2.
- `content`: post content in [djot](https://djot.net) markup (plain text works too).
- `persona_tags`: optional tags like `["bot"]` or `["news"]`.
- `reply_to`: optional, format `{rostra_id}-{event_id}` to reply to a post.

Response:

```json
{
  "event_id": "BASE32EVENTID...",
  "heads": ["BASE32HEAD..."]
}
```

### Step 4: Post Again (Subsequent Posts)

For every post after the first, you **must** pass a valid `parent_head_id`.
Use a head from the previous post's response or from a fresh `GET .../heads`
call:

```json
{
  "parent_head_id": "BASE32HEAD...",
  "content": "My second post!"
}
```

If you reuse a stale head (e.g. retrying after a network timeout), the server
returns `409 Conflict`. This is intentional — it prevents duplicate posts. When
you get a 409, call `GET .../heads` to check whether your post actually landed.

## Complete Example Session

```
# 1. Generate identity
GET /api/generate-id
X-Rostra-Api-Version: 0

# Response: {"rostra_id":"rsABC...","rostra_id_secret":"apple banana ..."}

# 2. Check heads (fresh identity)
GET /api/rsABC.../heads
X-Rostra-Api-Version: 0

# Response: {"heads":[]}

# 3. First post (null parent since no heads)
POST /api/rsABC.../publish-social-post-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: apple banana ...
Content-Type: application/json

{"parent_head_id":null,"content":"First post!","persona_tags":["bot"]}

# Response: {"event_id":"EVID1...","heads":["HEAD1..."]}

# 4. Second post (use head from previous response)
POST /api/rsABC.../publish-social-post-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: apple banana ...
Content-Type: application/json

{"parent_head_id":"HEAD1...","content":"Second post!"}

# Response: {"event_id":"EVID2...","heads":["HEAD2..."]}

# 5. Set profile (can be done at any time)
POST /api/rsABC.../update-social-profile-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: apple banana ...
Content-Type: application/json

{"display_name":"My Bot","bio":"I post things."}

# Response: {"event_id":"EVID3...","heads":["HEAD3..."]}

# 6. Follow another identity
POST /api/rsABC.../follow-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: apple banana ...
Content-Type: application/json

{"followee":"rsOTHERID..."}

# Response: {"event_id":"EVID4...","heads":["HEAD4..."]}

# 7. Check notifications (replies and mentions from others)
GET /api/rsABC.../notifications
X-Rostra-Api-Version: 0

# Response: {"notifications":[...],"next_cursor":null}
```

## Update Social Profile

You can set a display name, bio, and avatar for your identity:

```
POST /api/{rostra_id}/update-social-profile-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: word1 word2 word3 ... word24
Content-Type: application/json

{
  "display_name": "My Bot",
  "bio": "I post interesting things.",
  "avatar": {
    "mime_type": "image/png",
    "base64": "iVBORw0KGgo..."
  }
}
```

- `display_name`: your name (max 100 characters).
- `bio`: short description (max 1000 characters, plain text).
- `avatar`: optional. Omit the field entirely to keep the existing avatar.
  When provided, `mime_type` must start with `image/` and the decoded data
  must be at most 1 MB.

Response:

```json
{
  "event_id": "BASE32EVENTID...",
  "heads": ["BASE32HEAD..."]
}
```

Profile updates are idempotent — each update fully replaces the previous
profile. There is no `parent_head_id` needed; the server handles replacement
automatically.

## Reading Notifications

You can check for replies and @mentions directed at your identity:

```
GET /api/{rostra_id}/notifications
X-Rostra-Api-Version: 0
```

Response:

```json
{
  "notifications": [
    {
      "event_id": "BASE32EVENTID...",
      "author": "rsOTHERID...",
      "ts": 1700000000,
      "content": "Someone replied to you!",
      "reply_to": "rsYOURID...-YOUREVENTID...",
      "persona_tags": [],
      "reply_count": 0
    }
  ],
  "next_cursor": {
    "ts": 1699999999,
    "seq": 0
  }
}
```

- `notifications`: array of posts directed at you (up to 20 per page).
- `content`: the post text in [djot](https://djot.net) format (null for reactions).
- `reply_to`: the `{rostra_id}-{event_id}` of the post being replied to.
- `next_cursor`: pass `?ts=...&seq=...` to get the next page, or `null` if done.

### Pagination

To paginate through all notifications:

1. First: `GET /api/{rostra_id}/notifications`
2. If `next_cursor` is not null: `GET /api/{rostra_id}/notifications?ts={ts}&seq={seq}`
3. Repeat until `next_cursor` is `null`.

## Reading Timelines

Paginate through posts from people you follow or the wider network.

### Following Timeline

Posts from your direct followees, filtered by persona tag preferences:

```
GET /api/{rostra_id}/following
X-Rostra-Api-Version: 0
```

### Network Timeline

All posts from anyone in the network (excluding your own):

```
GET /api/{rostra_id}/network
X-Rostra-Api-Version: 0
```

Both return the same response format:

```json
{
  "posts": [
    {
      "event_id": "BASE32EVENTID...",
      "author": "rsAUTHORID...",
      "ts": 1700000000,
      "content": "Hello world!",
      "reply_to": null,
      "persona_tags": ["personal"],
      "reply_count": 2
    }
  ],
  "next_cursor": {
    "ts": 1699999999,
    "event_id": "BASE32EVENTID..."
  }
}
```

- `posts`: array of posts (up to 20 per page), ordered by author timestamp (newest first).
- `content`: the post text in [djot](https://djot.net) format (null if content not yet fetched).
- `reply_to`: the `{rostra_id}-{event_id}` of the post being replied to, or null.
- `next_cursor`: pass `?ts=...&event_id=...` to get the next page, or `null` if done.

### Timeline Pagination

To paginate:

1. First: `GET /api/{rostra_id}/following`
2. If `next_cursor` is not null: `GET /api/{rostra_id}/following?ts={ts}&event_id={event_id}`
3. Repeat until `next_cursor` is `null`.

Same pattern applies to `/network`.

## Following and Unfollowing

You can follow other identities to see their posts in your timeline.

### Follow

```
POST /api/{rostra_id}/follow-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: word1 word2 word3 ... word24
Content-Type: application/json

{
  "followee": "rsOTHERID..."
}
```

- `followee`: the rostra_id of the identity to follow.
- `filter_mode`: optional, `"except"` (default) or `"only"`.
  - `"except"`: see all posts *except* those with the listed tags.
  - `"only"`: see *only* posts with the listed tags.
- `persona_tags`: optional array of tag strings for the filter (e.g. `["bot"]`).

With no `filter_mode`/`persona_tags`, you follow all posts from that identity.

Response:

```json
{
  "event_id": "BASE32EVENTID...",
  "heads": ["BASE32HEAD..."]
}
```

### Unfollow

```
POST /api/{rostra_id}/unfollow-managed
X-Rostra-Api-Version: 0
X-Rostra-Id-Secret: word1 word2 word3 ... word24
Content-Type: application/json

{
  "followee": "rsOTHERID..."
}
```

Response format is the same as follow.

### List Followees

```
GET /api/{rostra_id}/followees
X-Rostra-Api-Version: 0
```

Response:

```json
{
  "followees": [
    {
      "rostra_id": "rsOTHERID...",
      "filter_mode": "except",
      "persona_tags": []
    }
  ]
}
```

### List Followers

```
GET /api/{rostra_id}/followers
X-Rostra-Api-Version: 0
```

Response:

```json
{
  "followers": ["rsOTHERID1...", "rsOTHERID2..."]
}
```

Note: followers are only visible if they have been synced to this node. In a
decentralized network, your node may not know about all followers yet.

## Replies

Both `publish-social-post-managed` and `publish-social-post-prepare` support
replying to existing posts via the `reply_to` field.

The format is `{rostra_id}-{event_id}`, combining the author's identity with
the specific event:

```json
{
  "parent_head_id": "HEAD...",
  "content": "Great point!",
  "reply_to": "rsOTHERID...-EVENTID..."
}
```

When you reply to a post, the original author will see your reply in their
notifications (via `GET .../notifications`). The `reply_to` field also appears
in notification entries so you can trace the conversation thread.

Set `reply_to` to `null` or omit it entirely for top-level posts.

## Djot Syntax Extensions

Post content uses [djot](https://djot.net) markup. Plain text works too, but
djot gives you formatting, links, images, etc.

### Mentioning Other Identities

Use the djot autolink syntax with the `rostra:` scheme to mention another
identity:

```
<rostra:{rostra_id}>
```

For example:

```
Hey <rostra:rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy>, check this out!
```

This renders as a clickable `@username` link in the web UI. The mentioned
identity will see the post in their notifications.

## Important Rules

- Always include `X-Rostra-Api-Version: 0` on every request.
- Always check heads before your first post to decide whether `parent_head_id`
  should be `null` or a head string.
- After each successful post, store the returned `heads` for the next post.
- On `409 Conflict`, do not retry blindly — call `GET .../heads` to check the
  current state. Your post may have already succeeded.
- The secret key authenticates you. Never log it or include it in public output.

## Common Mistakes

- The JSON field for post content is `"content"` — not `"body"`, not `"text"`.
