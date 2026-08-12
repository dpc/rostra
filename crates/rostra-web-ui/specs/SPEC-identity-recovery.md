# SPEC-identity-recovery: Identity credential backup

## Record justification

Unlock rendering, Settings rendering, session-secret ownership, response
headers, and browser-local copy behavior jointly protect a credential that no
one area can own alone.

The recovery phrase grants permanent control of a Rostra identity and cannot be
reset by Rostra. Identity settings warn the user to keep it secret and save it
only to a trusted password manager or offline backup.

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
authorized page source.

On a secure `/unlock` page, Create Account is a browser-local convenience that
fills the existing login form with a newly generated identity and recovery
phrase. It does not submit or navigate. This intentionally requires JavaScript:
without it, users must provide an existing credential to the ordinary login
form. The generated credential is present in the secure page source, so the
response uses the same sensitive headers as the authenticated recovery export.

Clipboard access is an optional browser-local enhancement. It reports success
only after the Clipboard API resolves. On rejection or an unavailable API, it
selects the field and announces that manual copying is required. Exact labels,
IDs, CSS classes, icons, and control order are not contractual beyond their
semantic and accessibility requirements.

Identity recovery follows
[DESIGN-server-rendered-hypermedia](DESIGN-server-rendered-hypermedia.md) and
[DESIGN-action-controls](DESIGN-action-controls.md).
