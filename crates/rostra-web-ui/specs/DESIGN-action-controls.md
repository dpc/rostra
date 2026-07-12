# DESIGN-action-controls: Labeled action controls

Status: inferred

The web UI's established primary-action style uses the shared `u-button`
presentation rendered by `fragment::button`. A conforming use pairs visible
text with the helper's component-specific `*ButtonIcon` hook and displays a
semantic SVG from `assets/icons`. Controls keep short, direct labels: actions
normally use a verb such as “Publish” or “Reveal”. A copy control may retain the
familiar visible value name, such as “RostraId”, when it also has an accessible
action name and state-aware icons communicate identity, copy availability, and
success.

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
