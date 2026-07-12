---
name: preview-rostra
description: >
  Use this skill to visually inspect Rostra's live development UI by navigating,
  activating a labelled control or element ID, scrolling, and taking screenshots
  without Python or JavaScript tooling.
user-invocable: true
---

# Preview Rostra

Use the project-local Rust CDP client against the live development server.

1. Check whether `http://[::1]:2345` is already running. Attach to it when it is.
   Otherwise ask the user to run `just dev-no-open` in a separate terminal.
   Never stop a server the inspection process did not start.
2. Capture the viewport and any needed interaction states:

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

3. Read each PNG with an image-capable tool. Do not claim visual inspection
   based only on a successful command.
4. Delete screenshots after reporting findings.

The browser profile is isolated and temporary, and its CDP endpoint is
loopback-only. Explicit navigation is restricted to the configured literal
loopback origin; leaving it through a redirect or control aborts the tool but
cannot prevent the initial request. Do not activate signing/destructive
controls, external links, or the recovery-phrase reveal flow. Loading live
development data can still update sessions and synchronization state. See
`docs/development-ui-preview.md` for lifecycle and troubleshooting details.
