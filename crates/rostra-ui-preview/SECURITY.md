# UI preview security and reliability

`rostra-ui-preview` is a development-only CLI. It runs with the invoking
user's filesystem and network permissions, launches Chromium, and drives a
live Rostra development site. Do not use it for production automation or
untrusted sites.

The site must be a literal loopback HTTP origin. Explicit navigation remains
at that origin; redirects and navigation caused by activated controls are
detected after they commit, so the initial external request cannot be
prevented. The loopback CDP endpoint is unauthenticated to other local
processes. This is an accepted tradeoff for this non-sensitive development
tool; do not use it on a shared host with sensitive page data.

Profiles are temporary and screenshots can contain live UI and session data.
Loading or activating the site can mutate sessions and synchronized local
state. Do not reveal recovery phrases or activate signing, destructive, or
external-link controls.

Chromium ownership begins immediately after `spawn`. Post-spawn errors and
normal exits must kill and reap the browser before deleting its profile.
Navigation readiness must remain tied to the requested page lifecycle, and
CDP operations must stay bounded. Revisit these assumptions whenever the CDP
transport, browser flags, origin policy, supported actions, or process
lifecycle changes.
