# Rostra Web UI API

The Web UI exposes a programmatic API at `/api/` for external tools and bots
that cannot execute Rust code directly and rely on a hosted instance of the
Rostra Web UI.

## Versioning

All requests **must** include the header:

```
X-Rostra-Api-Version: 0
```

The server rejects requests with a missing or unsupported version. This allows
future breaking changes without silently misinterpreting older clients.

## Authentication

Endpoints that modify data require the identity's secret key passed as a header:

```
X-Rostra-Id-Secret: <BIP39 24-word mnemonic>
```

The `-managed` suffix on endpoint names (e.g. `publish-social-post-managed`)
indicates that the secret key is sent to the backend, which signs the event on
the caller's behalf. A future API will support externally-signed events where
the secret key never leaves the client; this managed variant exists as a
pragmatic starting point for bots and scripts running against a trusted server.

## Error Format

All errors return JSON:

```json
{
  "error": "Human-readable error description"
}
```

with an appropriate HTTP status code (400, 401, 403, 409, 500).

## Endpoints

### `GET /api/generate-id`

Generate a new Rostra identity (keypair).

**Headers:** `X-Rostra-Api-Version: 0`

**Response (200):**

```json
{
  "rostra_id": "rs...",
  "rostra_id_secret": "word1 word2 ... word24"
}
```

- `rostra_id` — z32-encoded public key
- `rostra_id_secret` — BIP39 mnemonic (24 words); store securely

### `GET /api/{rostra_id}/heads`

Get the current head event IDs for an identity. Returns up to 10 heads, sorted
lexicographically.

**Headers:** `X-Rostra-Api-Version: 0`

**Path parameters:**

- `rostra_id` — the identity to query

**Response (200):**

```json
{
  "heads": ["BASE32ENCODED...", ...]
}
```

For a fresh (never-posted) identity, `heads` will be an empty array.

### `POST /api/{rostra_id}/publish-social-post-managed`

Publish a social post. The server signs the event on behalf of the caller (see
the note on `-managed` above).

**Headers:**

- `X-Rostra-Api-Version: 0`
- `X-Rostra-Id-Secret: <BIP39 mnemonic>` (must match `{rostra_id}`)
- `Content-Type: application/json`

**Path parameters:**

- `rostra_id` — the identity publishing the post

**Request body:**

```json
{
  "parent_head_id": "BASE32ENCODED..." | null,
  "persona_tags": ["bot", "news"],
  "content": "Hello, world!",
  "reply_to": "rsABCD...-EVENTID..." | null
}
```

| Field             | Type              | Required | Description                                              |
| ----------------- | ----------------- | -------- | -------------------------------------------------------- |
| `parent_head_id`  | `string` or `null`| yes      | Idempotence/consistency key (see below)                  |
| `persona_tags`    | `string[]`        | no       | Tags for the post (default: `[]`)                        |
| `content`         | `string`          | yes      | Post content in [djot](https://djot.net) format          |
| `reply_to`        | `string` or `null`| no       | `ExternalEventId` (`{rostra_id}-{event_id}`) to reply to |

**Response (200):**

```json
{
  "event_id": "BASE32ENCODED...",
  "heads": ["BASE32ENCODED...", ...]
}
```

- `event_id` — the `ShortEventId` of the newly created event
- `heads` — updated heads after the post

**Errors:**

| Status | Condition                                       |
| ------ | ----------------------------------------------- |
| 400    | Invalid request body, bad `parent_head_id` format, bad `reply_to` |
| 401    | Missing `X-Rostra-Id-Secret` header             |
| 403    | Secret does not match `{rostra_id}`             |
| 409    | `parent_head_id` consistency check failed       |

#### Idempotence via `parent_head_id`

The `parent_head_id` field serves as both a consistency check and an
idempotence key. It represents the caller's view of the identity's current
head.

1. `parent_head_id` is `null` and the identity **has** existing heads: **reject
   (409)**. The caller must acknowledge existing state.
2. `parent_head_id` is a string but is **not** among the current heads:
   **reject (409)**. The caller's view is stale — the post may have already
   been created.
3. `parent_head_id` is `null` and there are **no** heads: **proceed** (first
   post ever).
4. `parent_head_id` matches one of the current heads: **proceed**.

**Typical flow:**

1. Call `GET /api/{id}/heads` to learn the current heads.
2. Call `POST .../publish-social-post-managed` with one of the returned heads
   (or `null` if the array was empty).
3. If you get a 409, call `GET /api/{id}/heads` again to check whether your
   post actually landed (the head changed) or whether someone else posted.

This guarantees at-most-once delivery: if a caller retries the same request
after a network timeout, the second attempt will fail with 409 because the
head has already changed.

## Example: curl

```bash
# Generate an identity
curl -s -H "X-Rostra-Api-Version: 0" http://localhost:2345/api/generate-id

# Check heads
curl -s -H "X-Rostra-Api-Version: 0" http://localhost:2345/api/$ID/heads

# Publish a post (first post, no existing heads)
curl -s -X POST \
  -H "X-Rostra-Api-Version: 0" \
  -H "X-Rostra-Id-Secret: word1 word2 ... word24" \
  -H "Content-Type: application/json" \
  -d '{"parent_head_id": null, "content": "Hello from API!"}' \
  http://localhost:2345/api/$ID/publish-social-post-managed
```
