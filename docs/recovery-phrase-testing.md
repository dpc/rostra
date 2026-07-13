# Recovery phrase clipboard verification

The server-side and ordinary-HTTP behavior is covered by
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
