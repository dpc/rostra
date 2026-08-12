# Recovery phrase clipboard verification

The server-side behavior is covered by
[`SPEC-identity-recovery`](../crates/rostra-web-ui/specs/SPEC-identity-recovery.md)
and the web UI smoke tests. Before release, exercise only the browser behavior
that HTTP tests cannot verify:

| Browser condition | Expected result |
| --- | --- |
| Clipboard permission granted | All 24 words paste into the chosen trusted destination; the polite status reports success. |
| Clipboard denied or API unavailable | The field is selected and the polite status truthfully requests manual copying. |

Repeat for a current Safari/WebKit browser and one Chromium or Firefox browser.
Confirm that the masked Settings field does not visually disclose the phrase
and remains keyboard-focusable. This is behavioral verification, not a
pixel-layout inspection.

On an unauthenticated secure `/unlock` page, click Create Account once and
confirm that it fills the existing Rostra ID and 24-word mnemonic fields, does
not submit the login form, and does not navigate. Do not repeat this check in a
live shared browser session: it creates a credential.

With JavaScript disabled, confirm that Create Account is unavailable while the
ordinary existing-credential login form remains usable.
