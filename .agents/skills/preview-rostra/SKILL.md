---
name: preview-rostra
description: >
  Use this skill to inspect Rostra's live development UI through structured
  rendered evidence or human-readable screenshots while navigating, activating
  controls, and scrolling without Python or JavaScript tooling.
user-invocable: true
---

# Preview Rostra

Use the project-local Rust CDP client against the live development server.

1. Check whether `http://[::1]:2345` is already running. Attach to it when it is.
   Otherwise ask the user to run `just dev-no-open` in a separate terminal.
   Never stop a server the inspection process did not start.
2. Prefer structured rendered evidence when no image-capable tool is available:

```bash
just ui-inspect --path /news <<'EOF'
inspect-label Settings
inspect-id self-profile-summary
EOF
```

`inspect-label` and `inspect-id` print bounded JSON containing browser-computed
accessibility name/role, rendered non-hidden text, classes, geometry, relevant computed
styles, disabled/pressed/focus/checked state, pseudo-element and SVG/icon
evidence, and rendered evidence for nearby elements (including a hidden input's
visible toggle sibling). Container evidence also includes up to 16 rendered
children through depth two with accessibility role/name, geometry, and key
spacing/typography styles. They omit form values but can include sensitive live
page text, labels, CSS URLs, and context; inspect only approved non-secret
regions and treat captured stdout as a retained artifact.
Use this evidence to compare controls, but call it **structured rendered
inspection**, not pixel-level visual inspection.

If an `inspect-label` lookup fails, the tool prints bounded nearby accessible
label suggestions. Missing labels and IDs continue later inspection actions,
skip later non-inspection actions for safety, and produce a nonzero exit after
the stream. Click, hover, navigation, authentication, and non-lookup inspection
failures still stop immediately.

3. Use real browser hover only when the state itself matters:

```bash
just ui-inspect --path /settings/identity <<'EOF'
hover-label Copy RostraId
inspect-label Copy RostraId
unhover
EOF
```

`hover-label` and `hover-id` scroll the target into view and move Chromium's
pointer to its center, so `:hover`, pseudo-elements, and transitions are real
browser state. Always `unhover` before unrelated evidence.

4. Capture PNGs only when an image-capable human or tool will actually read them:

```bash
just ui-inspect --path /news <<'EOF'
screenshot /tmp/rostra-news.png
scroll down
screenshot /tmp/rostra-news-scrolled.png
EOF
```

Use `click-label Accessible name` for a uniquely labelled control or
`click-id element-id` when an ID is the stable interface. Use `open /path` for
later navigation. Input lines execute from top to bottom. For mobile inspection
add `--width 390 --height 844` before the here-document.

5. Read each PNG with an image-capable tool. Do not claim visual inspection
   based only on a successful command.
6. Delete screenshots after reporting findings.

## Existing development identity

The fresh browser profile has no Rostra session. When authenticated Settings
inspection is necessary, first obtain explicit approval to unlock the existing
development identity. Never generate/recover an account and never put its
mnemonic in a command, argument, environment variable, prompt, log, or retained
artifact.

Ensure the normal development recipe has hardened the local files (`just
dev-no-open` now enforces directory mode 0700 and secret mode 0600), then pass
only the secret **path**:

```bash
just ui-inspect --allow-secret-input --path /unlock <<'EOF'
unlock-from-dev-secret dev/2345/secret
click-label Login
open /settings/identity
inspect-label Reveal
EOF
```

`unlock-from-dev-secret` is bound to the configured port's
`dev/<port>/secret`, the `/unlock` route, and Rostra's exact password control.
It rejects symlinked path components, special files, files readable by group or others,
empty/non-UTF-8 values, NUL bytes, and files over 16 KiB. It never
prints the value and structured inspection omits form values. Do not inspect or
screenshot the password control; click Login immediately after filling it.
Unlocking gives the browser signing authority and may start network-visible
client activity; close the inspector immediately after the approved evidence is
collected. The mnemonic traverses the unauthenticated loopback CDP connection
and exists briefly in browser memory, so use this only on a single-user host
with trusted local processes. Rostra page scripts receive input events and must
not reflect it into inspectable content. Do not activate the phrase-rendering
confirmation action. Every `--allow-secret-input` run automatically closes open
dialogs and posts Rostra logout on both success and ordinary action errors.

### Authorized confirmation-only inspection

The first Identity action labelled **Reveal** only opens a non-secret warning
dialog. Opening that warning is allowed only when confirmation-dialog inspection
was explicitly authorized. The dialog contains another action also labelled
**Reveal**; activating that second action submits the form and renders the
recovery phrase, and is forbidden in the normal preview workflow.

Use this exact open/inspect/cancel sequence. There is intentionally only one
`click-label Reveal`:

```bash
just ui-inspect --allow-secret-input --path /unlock <<'EOF'
unlock-from-dev-secret dev/2345/secret
click-label Login
open /settings/identity
click-label Reveal
inspect-id recovery-confirmation
inspect-id recovery-phrase-target
click-label Cancel
EOF
```

Do not add a second `click-label Reveal`, do not submit the confirmation form,
and do not screenshot or inspect any phrase panel. `Cancel` closes the warning
without rendering the phrase. The tool's authenticated cleanup also closes any
still-open dialog and logs out, including when either inspection lookup fails.
If Chromium/CDP itself is unavailable, cleanup can fail; report the error and
restart `just dev` to clear residual server-memory authority.

The browser profile is isolated and temporary, and its CDP endpoint is
loopback-only. Explicit navigation is restricted to the configured literal
loopback origin; leaving it through a redirect or control aborts the tool but
cannot prevent the initial request. Do not activate signing/destructive
controls, external links, or the phrase-rendering confirmation step. Loading live
development data can still update sessions and synchronization state. See
`docs/development-ui-preview.md` for lifecycle and troubleshooting details.

During `just dev` rebuilds, initial attach retries recognizable readiness for up
to roughly ten seconds and navigation retries a same-origin non-Rostra transient page
for up to three seconds. Other failures remain bounded and explicit.
