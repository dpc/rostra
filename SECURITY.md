# Agent-browser security boundaries

Rostra's `preview-rostra` skill drives a signing-capable development UI with an
external browser daemon. Treat the daemon, browser process, local host, page
content, and development identity as security boundaries rather than ordinary
test fixtures.

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

An authenticated browser grants signing authority and can start network-visible
activity. Rostra's masked identity page still contains the recovery phrase in
the DOM. Never open or inspect `/settings/identity`. Log out and verify
`/unlock` before closing. Browser or logout failure can leave server-memory
authority; close the task session, remove its vault entry, report the failure,
and restart `just dev` before relying on cleanup.
