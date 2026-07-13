# rostra-ui-preview agent guidance

Read `SECURITY.md` before changing this crate. Keep its subprocess, CDP
transport, navigation boundary, timeout, and temporary-profile invariants
synchronized with that document and the root repository instructions.

When changing Rostra's application description marker or `/unlock` form markup,
update `Browser::ensure_rostra_page`, `Browser::fill_rostra_unlock_password`,
their controlled-browser smoke coverage, and the preview documentation together.
Likewise, changes to Identity's `Reveal`/`Cancel` labels,
`recovery-confirmation`, or `recovery-phrase-target` must update the
confirmation-only skill recipe, security notes, and live verification together.
