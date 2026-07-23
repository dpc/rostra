# rostra-client-db agent guidance

Read `SECURITY.md`, the root `AGENTS.md`,
`specs/ARCH-client-database.md`, and
`specs/SPEC-event-content-lifecycle.md` before changing persistence,
migration, ingestion, transaction, publication, or extension-table code.

Keep source-table compatibility, stash retry behavior, transaction rollback,
resource bounds, and historical database fixtures synchronized with
`SECURITY.md`.
