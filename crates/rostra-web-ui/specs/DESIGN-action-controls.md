# DESIGN-action-controls: Labeled action controls

Status: confirmed, 2026-07-12, maintainer

The web UI's established primary-action style uses the shared `u-button`
presentation rendered by `fragment::button`. A conforming use pairs visible
text with the helper's component-specific `*ButtonIcon` hook and displays a
semantic SVG from `assets/icons`. Controls keep short, direct labels: actions
normally use a verb such as “Publish” or “Reveal”. A copy control may retain the
familiar visible value name, such as “RostraId”, when it also has an accessible
action name and state-aware icons communicate identity, copy availability, and
success.

Shared buttons retain a bounded width and may wrap by default. A specific
control may opt into single-line growth when wrapping would obscure a concise
action label and its container can safely accommodate the wider control. Copy
actions use a concise visible label such as “Copy”, a contextual accessible
name when needed, and the established copy icon rather than a long wrapping
label.

## Rationale

The shared helper keeps dimensions, spacing, interaction states, and icon
placement consistent across routes. Component-specific icon hooks allow the
icon to reflect local meaning or state without duplicating the underlying
button layout. Text remains present for accessibility and clarity, while terse
labels prevent the bounded button width from producing unusually tall
controls.

This decision governs controls that opt into the primary-action style; it does
not claim that every historical or specialized button already does so. A new
primary action should not introduce a local plain-button pattern or leave the
helper's icon hook without a rendered icon.

Visual consistency does not override the semantic link, form, and no-JavaScript
requirements in
[DESIGN-server-rendered-hypermedia](DESIGN-server-rendered-hypermedia.md).
