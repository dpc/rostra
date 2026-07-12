# Recovery phrase browser verification

The recovery phrase UI deliberately uses browser APIs that HTTP-level tests
cannot fully exercise. Before release, repeat this matrix against
Settings → Identity & recovery and the Create Account flow.

| Platform | Clipboard allowed | Clipboard denied/unavailable |
| --- | --- | --- |
| Current iOS Safari | Copy, switch to a password manager, and paste all 24 words | Confirm fallback selects all 24 words and manual Copy works |
| Current macOS Safari | Copy and paste all 24 words | Confirm `execCommand` fallback or selected manual Copy works |
| Current Chrome | Copy and paste all 24 words | Confirm fallback selection and non-secret guidance |
| Current Firefox | Copy and paste all 24 words | Confirm fallback selection and non-secret guidance |

For each platform:

1. Confirm the Settings page source does not contain the phrase before reveal.
2. Confirm reveal requires opening and accepting the warning dialog.
3. Confirm the phrase can be selected directly in the read-only textarea.
4. Confirm Copy reports success without displaying the phrase elsewhere.
5. Switch to a password manager and paste; changing app focus must not erase it.
6. Press Hide and confirm the textarea is removed from the DOM.
7. Reveal again, wait two minutes without interacting, and confirm DOM removal.
8. Reveal again, navigate away and back (including BFCache), and confirm the
   phrase does not reappear without another reveal.
9. During account creation, confirm Continue stays disabled until “I saved this
   recovery phrase” is checked.
10. Confirm non-loopback HTTP disables both account generation and Settings
    reveal rather than relying on a browser warning.
