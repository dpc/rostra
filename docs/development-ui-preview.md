# Inspecting the live development UI

`rostra-ui-preview` is a small Rust CDP client for repeatable visual inspection.
It deliberately supports only navigation, activation by accessible label or
element ID, viewport scrolling, readiness waits, and screenshots. It does not
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
`ready`. Standard input is the most reliable interface through `just` and
executes one action per line; blank lines and lines beginning with `#` are
ignored. The binary also accepts repeated `--action 'COMMAND'` options when
called directly. The default viewport is 1280×900; use
`--width 390 --height 844` for a typical mobile viewport. With `--headed` and
an interactive terminal, enter one action per line and press Ctrl-D to close
the browser. Run `just ui-inspect --help` for all flags.

The command checks the existing `http://[::1]:2345` endpoint before starting
Chromium and never owns or stops `just dev`. This attach-first split means
cleanup cannot accidentally terminate another developer's server. A custom
literal loopback origin can be supplied with `--origin`.

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
- The tool waits for a complete document, loaded fonts, and two animation
  frames. During a hot-reload disconnect, rerun the command after `just dev`
  finishes rebuilding.

Chromium diagnostics are intentionally suppressed; CDP endpoint failure is
reported with a generic bounded error. A site-readiness failure includes a
reminder to start `just dev-no-open`.

The crate's [security and reliability notes](../crates/rostra-ui-preview/SECURITY.md)
record the accepted local-CDP tradeoff and invariants to revisit when changing
the tool.

## Testing and verification

Parser and loopback-origin unit tests run with:

```console
$ cargo test -p rostra-ui-preview
```

The ignored smoke test starts a controlled loopback HTML fixture, launches the
pinned Chromium, exercises both activation methods and scrolling, and verifies
a PNG capture. Run it after changing CDP, Chromium, or lifecycle code:

```console
$ cargo test -p rostra-ui-preview browser::tests::chromium_smoke -- --ignored
```

This smoke test does not start Rostra or inspect live data. Before relying on
the workflow for UI work, also run the harmless `/news` → `Settings` example
above against `just dev-no-open`.
