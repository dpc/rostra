# Agent-browser security boundaries

Rostra's `preview-rostra` skill drives a signing-capable development UI with an
external browser daemon. Treat the daemon, browser process, local host, page
content, and development identity as security boundaries rather than ordinary
test fixtures.

## Canonical resource URL resolution

First-party UI routes accept legacy full RostraId and EventId path components,
but generate short forms. Resolve a short RostraId only through the retained
identity index. Resolve a full EventId through its short retained-event key and
verify that the retained signed envelope recomputes to that exact full ID before
shortening it. Never treat a matching short prefix as proof of a full EventId.

Before redirecting or serving an author-scoped resource, bind the route author
to the retained envelope and, where applicable, to the materialized social-post
record. GET and HEAD requests that use a legacy form return a 308 canonical URL
without discarding the query string. Mutation POST routes resolve either form
in place; they must not rely on a redirect to preserve the request body.

## User-controlled media responses

Social-media event bytes and profile avatar bytes are untrusted even when their
event signatures verify. An avatar must declare and contain one of AVIF, BMP,
GIF, ICO, JPEG, PNG, SVG, TIFF, or WebP. SVG is fully parsed without a document
type and is limited to the existing 1 MB avatar limit. Validate locally
submitted avatars and repeat validation when serving retained data; signed
event compatibility still permits historical malformed declarations.

The content renderer and `/media/...` route only embed and serve inline a
declared type when its detected bytes match the intentional passive set: those
image formats, plus MP4 and WebM video. Every other generic media response,
including a MIME mismatch, unknown binary data, and active content, uses
`application/octet-stream` and `Content-Disposition: attachment` with the
fixed `rostra-media.bin` filename. Never derive a download filename from an
author declaration or post text.

All avatar and generic-media responses use `X-Content-Type-Options: nosniff`
and a sandboxing Content Security Policy that denies default sources, base
URLs, and form submission. Set these headers before conditional ETag handling
so `304 Not Modified` preserves the security policy and representation
metadata, including attachment disposition.

The skill wrapper loads a known-empty configuration and clears environment
settings that could silently select persistent profiles or state, remote CDP or
cloud providers, proxies, extensions, init scripts, plugins, and browser
arguments. Do not bypass the wrapper. Refuse a pre-existing task session because
launch-time restrictions cannot be retrofitted reliably.

Agent-browser's domain allowlist accepts the IPv6 host `[::1]`, not an exact
port. It therefore permits navigation to every HTTP service on that loopback
host. Inspect link targets and form actions before authenticated activation,
verify the exact origin after navigation, and do not treat the allowlist as an
origin boundary.

Agent-browser controls Chromium and its daemon with the invoking user's
privileges. A browser, proxy, extension, init script, plugin, provider, or remote
CDP endpoint can observe credentials and page data. Use only the local Chromium
launched through the skill wrapper on a trusted single-user host.

Agent-browser's auth vault encrypts credentials, but stores the vault and its
decryption key under the same user account. A task-scoped vault entry is a
temporary duplicate of the development mnemonic, not an independent security
boundary. Create one only after explicit approval, delete it immediately after
the login attempt, never reuse it, and report interrupted cleanup.

Snapshots, screenshots, downloads, traces, state files, and browser profiles can
contain live or secret data. The skill forbids plaintext state and persistent
profiles by default. Create approved artifacts with owner-only permissions under
a task-unique directory, inspect only what is needed, and remove artifacts on
success and handled failure.

Every secure non-AJAX `/unlock` render embeds a freshly generated, unused
credential so Create Account can fill the login fields without a request. This
makes merely opening that page credential-bearing: browser processes, same-origin
scripts, proxies, extensions, snapshots, screenshots, page source, and DOM
inspection can receive the mnemonic. Sensitive response headers reduce caching
and framing risks but do not prevent that exposure. Use `/unlock` only on the
trusted local single-user host, do not inspect or capture it, and authenticate
immediately when it is unavoidable. This accepted risk is inherent in the chosen
in-place interaction; revisit it if `/unlock` gains third-party scripts, if the
browser isolation model changes, or if another account-creation flow is added.

An authenticated browser grants signing authority and can start network-visible
activity. Rostra's masked identity page still contains the recovery phrase in
the DOM. Never open or inspect `/settings/identity`. Log out and verify
`/unlock` before closing. Browser or logout failure can leave server-memory
authority; close the task session, remove its vault entry, report the failure,
and restart `just dev` before relying on cleanup.
