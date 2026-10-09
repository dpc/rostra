---
name: multipart-review-ui
description: Must apply when reviewing Rostra web UI changes to controls, forms, layout, or user-facing interactions, whether in direct general review or delegated UI review.
only-roles: [reviewer]
---

# Rostra UI Review

Before starting, read `multipart-review` and follow its common contract. This is
a project-specific focused review extension, not a separate review workflow.

## Scope

Review the changed UI and its ordinary HTTP and enhanced interaction paths.
Use the [UI design guide](../../../crates/rostra-web-ui/design-ui.md) as context
for the current visual language and maintenance guidance, not as a new source of
stakeholder mandates.
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
  conventions. For new or materially changed controls, compare the actual
  equivalent controls, shared helper implementations, and relevant styles,
  rather than assuming that global styles make an ad hoc control equivalent.
  Buttons should use the established helpers and icon patterns
  where applicable; flag missing icons or ad hoc controls inconsistent with
  their peers. Respect legitimate existing exceptions rather than demanding
  an icon on every button or making incidental markup a mandate. Establish an
  exception with a comparable control or documented rationale that fits the
  changed control's role; an assumed exception cannot justify PASS. Report
  unresolved evidence that blocks a verdict through the common contract.
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
- **Claimed UX outcome:** Verify the intended user-visible improvement, not
  merely that the interaction functions. Choose concrete boundary examples
  proportionate to the changed workflow: an enlarged image should remain
  viewable and dismissible when taller than the viewport; a widening composer
  should remain within its available workspace. Use relevant source, styles,
  or authorized inspection as evidence, without imposing an unrelated test
  matrix or requiring browser access.

## Findings

Use the multipart review reporting contract. For each finding, identify the
changed element or response path, the user-visible failure, and the requested
outcome. For consistency findings, cite a comparable existing component or
pattern so the fix is concrete rather than a personal redesign preference.
