# DESIGN-server-rendered-hypermedia: HTML-first web workflows

Status: confirmed, 2026-07-12, user

Axum and Maud server-rendered HTML, standard links, and standard HTTP forms are
the primary web UI architecture. The server owns application data, validation,
authorization, and workflow transitions. Core workflows must work without
JavaScript: an ordinary request returns a complete usable page or a redirect.
Alpine-ajax may progressively enhance the same workflow with identified,
server-rendered fragments.

Alpine and alpine-ajax are the approved enhancement mechanism. Client code is
reserved for small browser-local behavior that HTML and HTTP cannot reasonably
provide, such as clipboard access, focus and keyboard conveniences, or upload
progress. Custom fetch/XHR, client-rendered markup, duplicated authoritative
state, and client-side workflow state machines require explicit justification.
JavaScript is never a security boundary, and a dialog cannot be the only way to
reach a core action.

Prefer one server-owned step or page to chained overlays and client-managed
stages. An exception must identify the affected flow, explain why it is needed,
and define its no-JavaScript behavior. Enhanced behavior may be less immediate;
correctness, inspectability, accessibility, and low client complexity take
priority.

## Review checklist

For each new or materially changed action, review:

1. Its no-JavaScript link/form behavior and ordinary full-page response.
2. Whether returned state and markup are server-rendered, including a tested
   ordinary path for any Alpine-targeted response.
3. Why each client behavior cannot reasonably use HTML/HTTP, and whether it
   remains small, browser-local, and non-authoritative.
4. Whether a dialog or client-managed stage is necessary and avoidable.
5. Whether authorization, CSRF, validation, and secret handling are entirely
   server-enforced.
6. Whether tests cover semantic behavior rather than incidental classes, IDs,
   wording, or control order.
7. Agreement with this decision and the feature's Linked Specs, with any
   exception explicit and scoped.
