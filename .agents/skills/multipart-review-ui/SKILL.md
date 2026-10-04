---
name: multipart-review-ui
description: Use when reviewing Rostra web UI changes to controls, forms, layout, or user-facing interactions.
only-roles: [reviewer]
---

# Rostra UI Review

Before starting, read `multipart-review` and follow its common contract. This is
a project-specific focused review extension, not a separate review workflow.

## Scope

Review the changed UI and its ordinary HTTP and enhanced interaction paths.
Read comparable existing screens, shared helpers in
`crates/rostra-web-ui/src/routes/fragment.rs`, and relevant styles before judging
consistency. Apply
`crates/rostra-web-ui/specs/DESIGN-server-rendered-hypermedia.md` and
`crates/rostra-web-ui/specs/REQ-ui-functionality-security.md`.

Do not assume browser access or permission to submit forms. If authorized browser
inspection would resolve a concrete uncertainty, use `preview-rostra` and its
prerequisites; otherwise report the uncertainty when it blocks a verdict.

## Review priorities

- **Existing patterns:** Check all changed elements against comparable Rostra
  UI: shared components, styles, spacing, action placement, and interaction
  conventions. Buttons should use the established helpers and icon patterns
  where applicable; flag missing icons or ad hoc controls inconsistent with
  their peers. Respect legitimate existing exceptions rather than demanding
  an icon on every button or making incidental markup a mandate.
- **Text and layout:** Check that labels explain actions, primary and secondary
  actions are distinguishable, and content remains usable on narrow screens.
  Look for misleading wording, crowded controls, or layouts that obscure the
  next action.
- **Accessible interaction:** Check semantic links and buttons, accessible
  names (especially icon-only controls), associated form labels, keyboard
  operation, visible focus, focus recovery after fragment updates or dialogs,
  and usable touch targets. Keyboard shortcuts that call `requestSubmit()`
  must use `keyup`, not repeat-prone `keydown`.
- **Hypermedia form errors:** Check validation and submission failures through
  both ordinary HTTP and Alpine AJAX. Errors must use the existing
  server-rendered page/fragment patterns, reach the intended update target,
  and leave the form usable with actionable feedback and a retry path.
  Preserve appropriate user inputs without exposing credentials or private
  data. Flag JS-only alerts, silently lost errors or inputs, and full-page
  responses accidentally inserted into fragments. The ordinary HTTP path
  must remain usable without JavaScript, subject to the documented exceptions.
- **Interaction states:** Check loading, disabled, empty, success, and failure
  states where affected. Look for duplicate submissions, stale controls, or
  updates that leave users unable to continue. Do not reject required
  first-party browser conveniences merely for using Alpine.

## Findings

Use the multipart review reporting contract. For each finding, identify the
changed element or response path, the user-visible failure, and the requested
outcome. For consistency findings, cite a comparable existing component or
pattern so the fix is concrete rather than a personal redesign preference.
