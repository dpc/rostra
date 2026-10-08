# Web UI design

Rostra's UI is a compact, content-first social workspace: a restrained neutral
frame, warm links, recognizable people, and readable conversations. Extend that
language rather than treating each new route as a new visual identity.

This is a practical guide to the current implementation, not a redesign or a
new stakeholder requirement. Here, coherence comes from restraint and familiar
controls.

## Purpose, tone, and differentiation

- **Purpose:** help people read, publish, follow identities, and hold public or
  private conversations in a friend-to-friend network.
- **Tone:** utilitarian and conversational, with fairly dense navigation and
  generous room for the actual post. This is an application, not a promotional
  landing page.
- **Differentiation:** people and their content supply most of the visual
  character. Avatars, display names, persona metadata, threaded context, and
  rich text sit inside a quiet, consistent frame. Identity and network controls
  remain visible rather than being disguised as a conventional hosted account.
- **Complexity:** prefer precise spacing, hierarchy, and useful feedback over
  decorative effects or a separate client-side design framework.

These are interpretations of the existing UI, offered as maintenance guidance.
The governing decisions remain
[HTML-first workflows](specs/DESIGN-server-rendered-hypermedia.md),
[labeled action controls](specs/DESIGN-action-controls.md), and
[functionality within security boundaries](specs/REQ-ui-functionality-security.md).
For credentials and private or user-controlled content, also consult
[identity recovery](specs/SPEC-identity-recovery.md) and
[SECURITY.md](../../SECURITY.md).

## Visual foundations

The shared [stylesheet](assets/style.css) owns the visual vocabulary. Reuse its
semantic variables and component rules rather than copying resolved colors into
new route-specific styles.

### Typography and hierarchy

The body uses browser `sans-serif`, 16px, with a 1.6 line height. There is no
bespoke display typeface. General headings use weight 600 and a tighter 1.2 line
height; their scale runs from 2rem for `h1` to 1rem for `h6`. Headings inside posts
are deliberately smaller than page headings. Code and technical identity values
use monospace; code blocks scroll horizontally instead of widening the page.

Post authors use bold display names next to small avatars. Timestamps and persona
metadata are secondary, commonly 0.8rem and muted. Body content carries more
weight than its metadata. Preserve this hierarchy instead of giving every label
the same size, contrast, or emphasis.

### Theme and tokens

Light and dark themes follow `prefers-color-scheme`; the page declares support
for both. The palette is mainly neutral, with warm orange-red links. These are
the active token families, not a complete token catalog:

| Role | Tokens and current treatment |
| --- | --- |
| Page and reading surfaces | `--color-bg-default`: light `oklch(97% 0 0)`, dark `#1c1c1c`; `--color-timeline-bg`: light 92% neutral OKLCH, dark `#282828` |
| Raised/content surfaces | `--color-post-bg`: light `#f7f7f7`, dark `#404040`; use the existing component's surface, not a new card on every element |
| Selection and navigation | `--color-post-highlight-bg` and `--color-tab-inactive-bg`; dark mode uses `#333333` for both |
| Text and derived surfaces | `--color-text-default`, `--color-pre-bg`, `--color-text-input-bg`, and `--color-timeline-item-border` derive from the `--color-bg` / `--color-fg` families |
| Links | `--color-link`: light `oklch(40% 0.20 30)`, dark `oklch(68% 0.12 30)`; `--color-link-hover` supplies hover feedback |
| Controls | `--color-button-bg`, `--color-button-border`, and their hover variants keep ordinary buttons neutral |
| Edges and depth | `--border-radius-std: 4px`, `--color-border`, `--shadow-color`, and `--shadow-color-strong` |
| Monochrome icons | `--invert` adapts the shared icon assets to the theme |

Do not confuse the older `--link-color` / `--link-hover-color` declarations with
the `--color-link` family used by ordinary anchors. Likewise, the declared
`--layout-break-width` is not what drives media queries: those use literal
breakpoints.

Borders, small radii, and related gray surfaces do most of the grouping.
Shadows are local accents, not a universal floating-card treatment. Gradients
have specific uses, such as fading folded reply context and decorating the
welcome heading; they are not the application's default background.

## Composition and responsive behavior

The normal workspace has a navigation/profile/composer column and a reading
column. At 768px and above, the layout becomes a row: navigation is sticky at
the top, between 280px and 440px wide, while the main column is capped at 50rem
and constrained to leave room for navigation. Below that breakpoint, the normal
layout stacks and the main column takes the available width.

The timeline is one bordered reading surface with lightly separated entries,
not a grid of detached cards. Spacing mixes `rem`, `em`, `pt`, and responsive
`clamp()` values; there is no universal spacing-token scale. Follow neighboring
components rather than inventing a new scale or opportunistically normalizing
unrelated styles.

The top of the reading surface uses a shared tab strip with a back affordance,
destination icons, active state, and unread counts. These are navigation links,
not a client-only tab widget. Profile pages add the viewed person's display
name as the current profile tab. Narrow navigation can visually hide labels
while retaining their accessible text.

Private messages reuse the same palette and controls but have a conversation
list and thread workspace. At mobile widths, an open thread hides the
conversation-list panel and provides a back link. Incoming and outgoing messages
are offset to opposite sides; outgoing messages also use `--color-pre-bg`. Do not force every route
into the timeline's exact arrangement when an existing workflow has its own
responsive composition.

When extending a layout, test long display names, identity strings, many tabs,
wide code, rich media, and narrow viewports. Use wrapping and `min-width: 0`
where needed; hiding essential content is not a substitute for fitting it.

## Content and component language

- **Posts:** preserve the avatar/name/content/action hierarchy and distinguish
  reply context from the focal post. Folded parent context uses truncation and a
  fade so that it does not dominate the reply.
- **Rich content:** rendered Djot supports prose, quotations, code, and media.
  Quotes have a left rule and a separate surface; code has a neutral background.
  External-media placeholders expose a load affordance, with titles presented
  as italic captions. Loading remote media is an explicit user choice, not a
  decorative preview request made on page load.
- **Primary actions:** use `fragment::button` with `u-button`, a short visible
  label, and a meaningful component-specific SVG icon hook. The default button
  has a 5rem minimum and 140px maximum width and may wrap. A justified local
  single-line exception is better than a long, awkwardly stacked label.
- **Specialized controls:** compact composer tools and reaction controls already
  have their own patterns. The primary-button convention does not mean every
  existing control is identical or should be replaced.
- **Forms:** keep the input, explanation, validation feedback, and submit action
  together. Reuse shared form/button helpers and the nearby composer layout.
  Placeholders supplement labels; they should not become the only explanation.
- **Feedback:** distinguish disabled, pending, success, and error states. Shared
  AJAX buttons delay their spinner by 150ms to avoid flashing it for fast work.
  Copy controls use state-aware icons and textual status rather than claiming
  success before the clipboard operation completes.

The main implementation entry points are [shared fragments](src/routes/fragment.rs),
[page layout](src/layout.rs), and the route's existing templates. CSS names
typically use `o-` for layout objects, `m-` for components, `u-` for utilities,
`__` for parts, and `-` modifiers for state. Continue the surrounding naming
pattern; do not create a second component system beside it.

## Interaction, motion, and accessibility

Axum and Maud own the page and its state transitions. Real links and forms are
the baseline; Alpine and alpine-ajax enhance server-rendered markup with
fragment updates and small browser-local conveniences. A modal or script must
not become the only way to perform a core action. The explicit exception is
`/unlock` Create Account, which fills the login form locally without submitting
or navigating; the linked workflow and recovery specs define that boundary.

Post and private-message composers have keyboard and draft conveniences.
Submission shortcuts use `keyup` with `requestSubmit()`, not repeating
`keydown` submission. Keep private-message drafts scoped to the account and
recipient. Keyboard timeline selection has its own highlight; mouse hover
highlighting is suppressed during keyboard navigation.

Motion is mostly feedback: link color/underline transitions, loading spinners,
brief welcome/unlock fades, notifications, and new shoutbox entry highlighting.
There is no general staggered page-reveal choreography. Reduced-motion rules
disable several animations and transitions, but do not cover every effect
(notably the shared spinner and global link transitions). Treat that as current
coverage, not a claim of a completed accessibility audit.

Preserve native semantics and keyboard operation. Existing patterns include
`aria-current` on active destinations, accessible names for icon-only back
links, visually hidden navigation labels at narrow widths, and component-level
`:focus-visible` outlines. For new work, check:

1. Focus remains visible and follows a sensible order after full-page or
   fragment updates; dialogs are keyboard-dismissable where used.
2. Actions have understandable names, fields have associated labels, and
   loading/error/success feedback is understandable without color alone.
3. Narrow screens, zoom, long content, touch input, and both color schemes remain
   usable. Do not infer contrast compliance from the presence of theme tokens:
   muted metadata and component-specific colors still need checking.
4. Reduced-motion preferences are respected by any new effect.
5. Core actions still work with JavaScript disabled; enhancement-only controls
   use the existing no-JavaScript handling rather than leaving dead buttons.

These are review checks for future work, not assertions that every historical
component already passes them.

## Preserve trust while polishing

Visual consistency never weakens authorization, safe content rendering, or
private-response protections. Do not add third-party scripts, fonts, or media
requests to make a private page look richer. Keep identity recovery a masked,
read-only field with conventional copy behavior and its warning; do not add
reveal dialogs or extra confirmation steps. Masking is presentation, not a
security boundary.

For a new screen, start from the nearest existing route, reuse its surfaces and
controls, and implement the ordinary HTTP path first. Then add only the
enhancements the workflow needs. Review the result in light/dark, wide/narrow,
keyboard, reduced-motion, and no-JavaScript conditions. Update this guide if the
shared visual language deliberately changes; change governing specs only when
the corresponding decision or requirement actually changes.
