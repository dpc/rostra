# UI preview security and reliability

`rostra-ui-preview` is a development-only CLI. It runs with the invoking
user's filesystem and network permissions, launches Chromium, and drives a
live Rostra development site. Do not use it for production automation or
untrusted sites.

The site must be a literal loopback HTTP origin. Explicit navigation remains
at that origin; redirects and navigation caused by activated controls are
detected after they commit, so the initial external request cannot be
prevented. The loopback CDP endpoint is unauthenticated to other local
processes. Ordinary inspection is development-only; authenticated inspection is
supported only on a single-user host where local processes are trusted.

Profiles are temporary and screenshots can contain live UI and session data.
Loading or activating the site can mutate sessions and synchronized local
state. Do not reveal recovery phrases or activate signing, destructive, or
external-link controls.

Secret-file entry is an explicit opt-in for an approved existing development
identity. It is bound to this project's `dev/<port>/secret`, the verified
Rostra `/unlock` page, and its exact password input. Secret values must come
only from a small, regular, non-symlink, owner-only file and must never be
accepted in argv/environment, printed, or intentionally captured. Structured
inspection omits form values, but the trusted local Rostra page receives input
events and must not reflect the secret into page content. Unlocking grants
signing authority and may cause network-visible activity. The mnemonic
traverses CDP and exists briefly in browser/profile memory. Forced termination
or cleanup failure can leave a residual temporary profile.

Structured JSON may contain live page text, accessibility labels, CSS URLs, and
nearby context. It has no generic secret detector and must target only approved
non-secret regions; retain it with the same care as a screenshot.

Chromium ownership begins immediately after `spawn`. Post-spawn errors and
normal exits must kill and reap the browser before deleting its profile.
Navigation readiness must remain tied to the requested page lifecycle, and
CDP operations must stay bounded. Revisit these assumptions whenever the CDP
transport, browser flags, origin policy, supported actions, or process
lifecycle changes.

Initial attach retries remain bounded. Navigation retries only a loaded
same-origin document that lacks Rostra's marker, under one three-second budget;
cross-origin, protocol, readiness, and other CDP failures abort. Retries repeat
the requested GET, so inspected routes must be safe to load repeatedly. Only
exact `inspect-label` and `inspect-id` lookup misses are aggregated; after one,
later non-inspection actions are skipped. Hover succeeds only when Chromium
reports the resolved target in `:hover` state.
