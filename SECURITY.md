# Agent-browser security boundaries

## Durable message-device lifecycle commands

`Client::mutate_direct_message_device_idempotent` requires a matching caller
secret and full active, open client before reading any receipt. Runtime activity
is not UI authentication: web and native hosts must enforce their own requesting
session boundary. The lower-level database signing method is a trusted
integration primitive, not an authorization interface.

Persist one random request ID with its exact public intention before calling.
Retirement names one known nonretired own device ID, not a label or epoch key.
Enrollment requires either no local installation or one exact retired local ID;
it never revives that ID. Background auto-enrollment can make an absent
expectation stale. Accepted retries resolve before lifecycle and capacity checks
and return the original outcome, even after later retirement. Reusing an accepted
ID with different intent is a typed mismatch. A new stale request is a typed
conflict, not permission to retarget or silently refresh the expectation.

Schema-41 receipts store only exact public intent, public announcement and signed
envelope. Installation replacement, fresh independent epoch creation, signed
announcement admission and receipt commit in one serialized transaction. Failure
before commit rolls all of them back; cancellation or a lost acknowledgment does
not establish rollback. No pending private key crosses an async boundary.
Permanent receipts survive reopen and total rebuild outside payload quotas.
They reveal public operations and their local correlation, never message history
or secret keys. This does not add forensic erasure or storage encryption.

Success means local acceptance, not distributed compare-and-swap, delivery,
reachability or present eligibility. Retry protection is local to this database
lineage, not independent/restored copies. Permanent retirement still dominates
delayed active announcements; it does not retire another ID, erase history, or
extend/delete the old receive epochs. Existing raw/web methods retain their
previous behavior and do not acquire this new durable retry contract. Older
schema-40 binaries reject a schema-41 database instead of downgrading it.

## Native local known-person reads

`Database::search_known_people` traverses only the account's self/direct/two-hop
active follow keys. It never substitutes the retained short-identity index for
that graph. Raw checked keys, bounded nested continuation, eight profile
observations per call, 2 MiB borrowed profile preflight and 1024-byte copied names
bound local work without collecting the whole graph or allocating avatars.
The shared web subsequence scorer is unchanged; the native reader's unavailable
profile fields can reduce ranking coverage and carry a cumulative warning.
Exhaustion ends a live traversal, not a cross-call snapshot/global-ranking claim.

`lookup_known_person` checks exact self/direct membership before bounded
two-hop traversal. Incomplete is not unknown. Exact retained event-author
evidence and profile availability are separate from graph membership; short
prefix collisions do not authenticate a full ID. Projected profile source
envelopes are size-bounded and checked for signature, full author, kind,
timestamp and short event ID. Names remain local projection data, not newly
authenticated signed body data. Oversized/corrupt names can retain independent
source-validated timestamps. No profile body is fetched or hashed, and no bio or
avatar is returned. Raw missing-source or mismatched profiles are not identity
evidence.

Opaque in-process search and lookup cursors bind the query/target, account,
database open and projection epoch. They cannot be supplied through serialized
untrusted fields; hosts keep a bounded session slot and discard it on reset or
account changes. Concurrent mutations require explicit refresh, and repeated
calls do not start network discovery or scheduling. Hosts must bound concurrency
and avoid the UI executor; there is no hard CPU deadline or process RSS limit.

## Native static-media upload and reuse

`rostra-core::static_media` is an opt-in upload policy, not a change to historical
image readers or the web's generic upload limits. V1 fully decodes static
PNG/WebP, expands palettes/packed grayscale/transparency, preserves PNG 8/16-bit
samples and alpha, and writes fresh noninterlaced PNG with only IHDR/IDAT/IEND.
Input and output each have a 1,000,000-byte limit; dimensions are at most 2048 per
axis and 4,000,000 pixels. The output writer refuses excess bytes before growing
its vector. Decoded buffers are bounded at eight bytes per pixel. Codec scratch
is not a hard RSS limit and no CPU deadline is promised; hosts bound concurrency
and run normalization off the UI executor.

Untagged samples are interpreted as sRGB. PNG sRGB intent 0, gAMA 45455, and
cHRM `(31270,32900,64000,33000,30000,60000,15000,6000)` are accepted without
sample conversion; other declarations are explicitly unsupported. All ICC and
EXIF are unsupported, including benign/default profiles and orientation 1,
rather than silently dropping a potentially meaningful transform. WebP XMP and
unknown semantic chunks are also unsupported. PNG text, time, and physical
resolution metadata are discarded. No source metadata is inherited. Normalized
output is untagged sRGB by policy; downstream color management is not promised.

`Client::publish_media_idempotent` validates the metadata-free V1 PNG subset
before the first signature and uses a separate random non-singleton event nonce.
Persist its exact normalized bytes, MIME, policy version and random 16-byte
request ID before calling. Never normalize a pending retry: codec versions may
produce different bytes. Permanent per-account schema-40 media receipts store
only the exact-input digest and original signed envelope, atomically with payload
admission. Exact retry lookup precedes current decoder/capacity/lifecycle checks,
so deletion and pruning are not undone. Receipts survive total replay/reopen,
never expire, lie outside payload quotas, and permit input dictionary guesses.
Shutdown rejects new calls, but cancellation or lost acknowledgment of a started
transaction does not imply rollback. Success means local acceptance, not delivery
or deduplication across restored/independent databases.

`Database::list_own_media` scans the own-event index with bounded raw rows and
body reservations and returns metadata-only candidates with full event IDs.
It verifies retained signatures, author/kind/short identity, lifecycle, body hash
and strict CBOR shape without copying full image bytes or decoding pixels.
Header dimensions are explicitly unvalidated hints, not safe-image eligibility.
Always call `read_native_media` for the exact account/full ID before preview or
reuse insertion, and re-read on later display. Empty pages may have continuation.
Cursors are exclusive/live, account/open-instance/rebuild-bound, not durable
snapshots; refresh to discover concurrent inserts before an already-scanned key.
No network request, normalized rewrite, or new signature occurs during reuse.

## Native Djot presentation

`rostra-djot::native::render` emits only bounded literal text segments and typed
identity or credential-free HTTP(S) destinations, never HTML or media fetch
instructions. Hosts must display lossy-conversion warnings and disclose actual
destinations before external navigation. A link label does not authenticate its
destination. Short identity references stay unresolved until checked against the
host's retained identity index; unknown references remain inert.

The opt-in `render_with_media` uses that same parser and all its guards, adding
at most 32 ordered image anchors with literal alternative text. A typed
`rostra-media:` destination preserves short versus full event IDs; it does not
authorize retrieval. Credential-free HTTP(S) image URLs remain distinct, inert
external destinations, not navigation links or fetch instructions. Invalid
schemes and unknown references remain inert. The ordinary text-only renderer
continues to replace images with alternative text.

`rostra-client-db`'s opt-in `native-media` feature reads only retained signed
`SOCIAL_MEDIA` events. The caller must supply the actual retained source post's
author; captions and UI-supplied identity strings are not authority. This matches
the web's author-scoped media URLs: cross-author reuse is not accepted. A full
ID must match the recomputed retained envelope, never just its short prefix.
The reader rechecks the signature, author, kind, content length and hash, and
honors Missing, Deleted, Pruned and Invalid states in one read snapshot. No
network fetch, scheduling, signing, or write occurs.

Before materializing payload bytes, the reader checks a 1024-byte stored envelope
cap, a 2,000,128-byte signed CBOR body cap, and a body-plus-nine-byte raw storage
cap. It borrows the stored body and preflights the complete native CBOR subset
before allocating the at-most-1,000,000-byte media vector. This subset accepts
definite maps with exactly `m` (at most 64 UTF-8 bytes) and `d` (a definite array
of shortest-encoded unsigned bytes), in either order. Unknown fields, tags,
indefinite containers and alternative/nonminimal encodings are Unsupported,
not changes to protocol validity. Duplicates, trailing bytes and malformed data
are rejected. Existing web serialization uses this integer-array form; the
larger body cap accounts for its two encoded bytes per media byte.

Native signed images then pass the same static PNG/WebP presentation validator
as native avatars: 2048 per dimension, 4,000,000 pixels, no animation, complete
decode, exact MIME. Original bytes and metadata are preserved, not normalized.
Decode occurs after releasing the DB snapshot in a blocking section. Hosts must
bound concurrency, avoid the UI executor, require explicit tap-to-load, and
re-read lifecycle state on later/offline redisplay rather than treating cached
bytes as enduring authority. A snapshot read cannot revoke bytes already
returned before a concurrent deletion. Missing, unsupported, invalid, oversized
and mismatched content must be presented honestly. There is no CPU deadline or
process RSS bound, third-party fetch, or new persistent media cache.

The additive `read_native_media_opaque` shares those retained-envelope,
signature, exact identity, lifecycle, signed/stored-length, hash and complete
CBOR checks. It omits only the image MIME/raster policy for explicit opaque
export. Original bytes remain bounded at 1,000,000; the original MIME is at most
64 UTF-8 bytes of untrusted metadata, not validated syntax or a safe filename,
extension or content-handler choice. Availability does not promise safe
execution, opening or display, even when the declaration says PNG/WebP.
Image presentation and own-image listing retain their existing policies.
The same source-post-author and snapshot/re-read obligations apply.

The native renderer checks input size and a conservative recursive-block-opener
budget before constructing jotup's eager parser, then separately bounds event
depth/count and owned output. The guard is tied to the exact pinned jotup 0.10.0
recursive paths and must be re-audited on upgrade. It can reject flat lists and
literal code with many apparent block openers; it is not merely a nesting limit.
Limit failure returns no partial document. Source-only fallback must be explicitly
labelled, independently display-bounded, and noninteractive. Run rendering off
the UI executor with bounded concurrency; no CPU deadline or RSS ceiling is
claimed. This policy does not change the web renderer or signed-event format.

Remote SocialPost ingestion also uses this pre-parser guard in
`mention::contains_mention`. Over-budget content returns false without parsing:
the signed event and original content are retained normally, but mention
indexing/notifications may be omitted, including for more than 64 flat list
items. In-budget full/short matching remains unchanged. The separate excerpt
helpers and web HTML renderer are not covered by this guard.

## Browser boundary

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

## Private-message HTTP access

The functionality/security balance for these routes is governed by
[`REQ-ui-functionality-security`](crates/rostra-web-ui/specs/REQ-ui-functionality-security.md).
Every `/messages` and `/settings/messages` request requires the exact requesting
session's in-memory identity secret and a matching full active client. A second
session's activation, a public-only login, or an open database is not authority
to read plaintext or manage devices. POST forms require an independent random
session-bound synchronizer token before any mutation; it is not the session
credential. Full and short identity paths resolve through the ordinary retained
identity rules before use.

These workflows return ordinary complete HTML pages and 303 redirects and remain
usable without JavaScript. Thread composers use the same self-hosted Alpine
persistence mechanism as new-post forms for account-and-recipient-scoped browser
drafts and Ctrl+Enter submission. Message text uses the same sanitized Djot
renderer as posts. Renderer output remains non-executable, and the private-page
CSP prevents message content from initiating third-party media requests. All
response classes, including extractor errors, missing routes, and redirects,
use private/no-store caching, identity/no-compression, no-referrer, nosniff,
framing denial, and a restrictive CSP that permits only the required
same-origin scripts, CSS, fonts, images, and forms. It permits inline styles for
the self-hosted MathJax renderer, while sanitized message content cannot supply
style elements or attributes.
Alpine's AJAX requests have same-origin connect access, and its attribute
expression evaluator requires `unsafe-eval`; no inline or remote script source
is allowed. The message workflow does not copy plaintext into request URLs,
logs, delivery receipts, or decryption-result callbacks; rendered safe links
remain user-controlled navigation targets. Local plaintext, including
browser-persisted drafts, persists independently of ciphertext and epoch-key
deletion; this is not endpoint or forensic-storage confidentiality.

Open conversations refresh through the existing authenticated UI websocket and
ordinary authorized HTML requests, not a new periodic polling loop. The local
background DM worker publishes coalesced, content-free progress wakeups after
every committed serviced trial batch, whether it succeeds, misses, or discards
an invalid queued item. These wakeups contain no identities, plaintext,
arrival sequences, or decryption results, and never reach the p2p transport.
The websocket rechecks the exact session's in-memory secret and full-client
authority before forwarding a wakeup. Each resulting history request enforces
the usual private-message authorization and response protections independently.
Arrival cursors are independent of signed timestamps and session read markers;
bounded pages catch up without losing delayed messages or arrivals another tab
already read. Hidden conversations defer work until visible, overlapping
notifications coalesce, and navigation stops further requests. A lost websocket
or failed request can leave the view stale until a new wakeup, visibility change,
or ordinary reload; live updates are an enhancement, not delivery receipts.
Changes to notification payloads, their destination, or their relationship to
decryption outcomes must recheck this boundary and the receive-security proof.

The named script, CSP, and browser-storage details describe the current
implementation, not immutable stakeholder choices. Changes may use other
mechanisms only while preserving the governing requirement and the
authorization, escaping, response-protection, and leakage boundaries above.

## User-controlled media responses

The new-post Add link helper accepts credential-free HTTP(S) URLs and produces
escaped Djot, using the renderer's YouTube and Twitter/X post recognition.
Optional title retrieval requires the requesting session's in-memory
identity secret. It sends no incoming cookies, authorization, or referrer, and
does not use environment proxies. Only ports 80 and 443 and conservatively
validated public addresses are fetched. Every redirect repeats validation;
validated DNS answers are pinned for the actual connection to prevent DNS
rebinding. Retrieval is limited to three redirects, eight seconds overall,
256 KiB per response, and four concurrent requests. Recognized YouTube videos
use the fixed YouTube oEmbed JSON endpoint with a canonical video URL; only its
plain title is used, never its HTML, thumbnails, or other media fields. Recognized
Twitter/X posts use the fixed `publish.x.com/oembed` JSON endpoint with a canonical
post URL. Only plain text from the returned post paragraph becomes an editable,
bounded title; script, style, and template content is excluded, and returned HTML
is never rendered. Other
links retain HTML title extraction. Metadata is plain, bounded,
editable text, never markup or executable code. Retrieval failure leaves manual
insertion available. The request intentionally discloses the chosen URL to its
website from the Rostra server.

Twitter/X posts use the existing external-media load presentation. The first
explicit activation assigns a fixed HTTPS `platform.twitter.com/embed/Tweet.html`
URL to an iframe; no provider request occurs before that action. Only validated
numeric post IDs enter the URL. Provider scripts never run in the signing-capable
application document, and remote embed HTML is never accepted. The frame's
sandbox allows scripts and the provider's own origin, but not top navigation;
do not replace the fixed cross-origin URL with srcdoc or an app-origin wrapper
under these permissions. The frame suppresses referrers and requests X's
do-not-track mode, but expansion still discloses the viewer's IP address and
allows X's media requests. Private-page CSP blocks the initial third-party frame
navigation. Without JavaScript, the keyboard-accessible load link performs
ordinary navigation instead.

Post frames are responsive and accept height-only resize notifications. The
first-party listener requires the exact provider origin and the exact visible,
marked iframe's `contentWindow`, validates the `twttr.embed` resize notification
shape, and limits heights to 64–4096 pixels. Reported widths, element IDs, HTML,
URLs, and other provider actions confer no authority.

Social-media event bytes and profile avatar bytes are untrusted even when their
event signatures verify. An avatar must declare and contain one of AVIF, BMP,
GIF, ICO, JPEG, PNG, SVG, TIFF, or WebP. SVG is fully parsed without a document
type and is limited to the existing 1 MB avatar limit. Validate locally
submitted avatars and repeat validation when serving retained data; signed
event compatibility still permits historical malformed declarations.

Native hosts can opt into `rostra-core`'s `raster-avatar` feature and
`avatar::validate_raster_avatar` for a narrower static PNG/WebP policy on
replacement and presentation. It fully decodes bounded pixels, rejects
animation and incomplete containers, accepts only exact canonical MIME types,
and limits encoded data to 1,000,000 bytes, each dimension to 2048, and pixels to
4,000,000. PNG metadata text/ICC decompression is skipped. JPEG is unsupported
because evaluated decoders accept missing entropy data even in strict mode.
These are input/output bounds, not a CPU deadline
or process RSS ceiling; codec scratch allocations are not uniformly budgeted.
Hosts must bound concurrency and avoid decoding on the UI/async executor. The
validator does not rewrite bytes or sanitize metadata. An unsupported historical
avatar may be preserved byte-for-byte during unrelated profile edits, but must
not be displayed through the native raster path without passing validation.
This opt-in policy does not narrow web presentation or signed-event ingestion.

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
