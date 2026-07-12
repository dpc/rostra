# Inspecting the live development UI

`rostra-ui-preview` is a small Rust CDP client for repeatable visual inspection.
It deliberately supports only navigation, activation by accessible label or
element ID, viewport scrolling, readiness waits, structured element evidence,
screenshots, and explicitly approved existing-dev-identity unlock. It does not
introduce a Python, Node.js, WebDriver, or general browser-test project.

## Start and inspect

The inspector attaches to an already running development site. Start one
terminal without automatically opening your usual browser:

```console
$ just dev-no-open
```

Then inspect it from another terminal:

```console
$ just ui-inspect --path /news <<'EOF'
click-label Settings
screenshot /tmp/rostra-settings.png
EOF
```

Other actions are `open PATH`, `click-id ID`, `scroll up`, `scroll down`, and
`ready`. `inspect-label LABEL` and `inspect-id ID` emit bounded JSON with the
element's computed accessibility role/name, text, classes, geometry, selected
visual styles, state, pseudo-elements, SVG/icon evidence, and nearby context.
They intentionally omit form values. This supports honest structural/rendered
comparison when an agent cannot read pixels; it is not a substitute for
pixel-level visual judgment. Standard input is the most reliable interface through `just` and
executes one action per line; blank lines and lines beginning with `#` are
ignored. The binary also accepts repeated `--action 'COMMAND'` options when
called directly. The default viewport is 1280×900; use
`--width 390 --height 844` for a typical mobile viewport. With `--headed` and
an interactive terminal, enter one action per line and press Ctrl-D to close
the browser. Run `just ui-inspect --help` for all flags.

`hover-label LABEL` and `hover-id ID` scroll a target into view and move
Chromium's pointer to its center. Subsequent inspection therefore observes real
`:hover` computed styles, pseudo-elements, and transitions. Use `unhover` before
collecting an unrelated state.

Failed `inspect-label` lookups print up to five bounded nearby accessible-label
suggestions. Missing labels and IDs do not prevent later inspection actions
from running; later non-inspection actions are skipped for safety. The process
still exits nonzero with an aggregate failure count. Failures of click, hover,
navigation, authentication, or non-lookup inspection work remain immediate.

The command checks the existing `http://[::1]:2345` endpoint before starting
Chromium and never owns or stops `just dev`. This attach-first split means
cleanup cannot accidentally terminate another developer's server. A custom
literal loopback origin can be supplied with `--origin`.

Attach tolerates a transient hot-reload outage for up to roughly ten seconds. Each
navigation also retries for up to three seconds when the requested same-origin
document loads without Rostra's application marker, covering the short error
page race observed during `cargo watch` replacement. Cross-origin and protocol
failures are never retried.

## Safety and lifecycle

- Chromium comes from the pinned Nix development shell. Override its path with
  `ROSTRA_CHROMIUM` only when intentionally testing another installed build.
- Every normal run and handled error creates a private temporary Chromium
  profile, binds its CDP discovery endpoint to IPv4 loopback, and closes or
  kills and waits for the child before deleting the profile. Forced termination
  such as `SIGKILL` cannot clean up.
- Explicit navigation actions reject non-loopback origins and cross-origin
  targets. The tool also aborts if a redirect or activated control leaves the
  configured origin, but it cannot prevent the initial external request.
  Do not activate signing, destructive, recovery-phrase, or external-link
  controls.
- The default Rostra development profile has no signing authority, but loading
  the live site may still update its session and synchronized local database.
- Screenshots contain live page data. Keep them under `/tmp`, inspect them with
  an image-capable tool, and delete them when finished. Never reveal or capture
  the recovery phrase.
- Structured JSON can likewise contain live text, accessibility labels, CSS
  URLs, and nearby context. It omits form values but has no generic secret
  detector; retain or share it only as deliberately as a screenshot.
- The tool waits for a complete document, loaded fonts, and two animation
  frames. During a hot-reload disconnect, rerun the command after `just dev`
  finishes rebuilding.

Chromium diagnostics are intentionally suppressed; CDP endpoint failure is
reported with a generic bounded error. A site-readiness failure includes a
reminder to start `just dev-no-open`.

The crate's [security and reliability notes](../crates/rostra-ui-preview/SECURITY.md)
record the accepted local-CDP tradeoff and invariants to revisit when changing
the tool.

## Approved development-identity login

An isolated profile starts without an authenticated session. If inspection
requires authenticated Settings, obtain explicit approval to unlock the
existing development identity. Never generate a new account, reveal a recovery
phrase, or put the mnemonic in arguments, environment variables, command text,
logs, screenshots, or retained artifacts.

`just dev` and `just dev-no-open` enforce mode 0700 on `dev/<port>` and 0600 on
its existing or newly created `secret`. Then use the protected file path only:

```console
$ just ui-inspect --allow-secret-input --path /unlock <<'EOF'
unlock-from-dev-secret dev/2345/secret
click-label Login
open /settings/identity
inspect-label Reveal
EOF
```

The unlock action requires an explicit CLI opt-in and is hard-bound to the
configured port's `dev/<port>/secret`, the Rostra `/unlock` route, and its exact
password control. It requires a regular non-symlink file, owner-only
permissions, UTF-8 text, and a 16 KiB maximum. It sends the value without
printing it, and structured inspection omits form values. Only use this against
the trusted local Rostra development server: page scripts receive input events
and could reflect entered data into otherwise inspectable content. Unlocking
grants signing authority and may start network-visible client activity. Close
the process immediately after collecting approved non-secret evidence, and
never activate the recovery-phrase control.

The mnemonic traverses Chromium's unauthenticated loopback CDP connection and
exists briefly in browser/profile memory. Authenticated inspection is supported
only on a single-user host with trusted local processes. Forced termination or
cleanup failure can leave a residual temporary profile; remove any
`/tmp/rostra-ui-preview-*` directory before continuing.

## Testing and verification

Parser and loopback-origin unit tests run with:

```console
$ cargo test -p rostra-ui-preview
```

The ignored smoke test starts a controlled loopback HTML fixture, launches the
pinned Chromium, exercises both activation methods and scrolling, and verifies
structured inspection (including visible toggle context and output bounds), the
exact unlock control with redacted secret errors, transient marker recovery,
nearby label suggestions, real hover/unhover state, and a PNG capture. Run it
after changing CDP, Chromium, web-UI marker/unlock markup, or lifecycle code:

```console
$ cargo test -p rostra-ui-preview browser::tests::chromium_smoke -- --ignored
```

This smoke test does not start Rostra or inspect live data. Before relying on
the workflow for UI work, also run the harmless `/news` → `Settings` example
above against `just dev-no-open`.
