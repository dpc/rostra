# SPEC-identity-recovery: Identity credential backup

The recovery phrase grants permanent control of a Rostra identity and cannot be
reset by Rostra. Identity backup and account-generation surfaces warn the user
to keep it secret and save it only to a trusted password manager or offline
backup.

Identity settings include the credential only when all of these conditions
hold:

- the request has an authenticated current session;
- that exact session holds the matching identity's secret key and is therefore
  read-write; and
- the server's effective origin is HTTPS or loopback HTTP.

Read-only and insecure-transport responses from these backup surfaces contain
no secret. Their responses that do contain a recovery phrase use
`Cache-Control: no-store, private`, legacy
`Pragma: no-cache`, `Content-Encoding: identity`, `X-Frame-Options: DENY`, and a
Content Security Policy that denies framing.

Identity settings present the phrase in a labeled, masked, read-only field with
a conventional copy action and a clear warning. Masking reduces accidental
shoulder-surfing but is not a security boundary: the secret is present in the
authorized page source. Account creation presents the newly generated phrase in
a labeled, selectable, read-only field and submits it through an ordinary form
with a validated local redirect. Its ordinary generation request returns a
complete page, while an Alpine request may return the equivalent recovery panel.
Both workflows remain usable without JavaScript.

Clipboard access is an optional browser-local enhancement. It reports success
only after the Clipboard API resolves. On rejection or an unavailable API, it
selects the field and announces that manual copying is required. Exact labels,
IDs, CSS classes, icons, and control order are not contractual beyond their
semantic and accessibility requirements.

This workflow follows
[DESIGN-server-rendered-hypermedia](DESIGN-server-rendered-hypermedia.md) and
[DESIGN-action-controls](DESIGN-action-controls.md).
