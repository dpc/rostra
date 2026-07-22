# rostra-client agent guidance

Read `SECURITY.md` before changing this crate. Also read the root `AGENTS.md`
and the Linked Specs that govern the affected runtime or protocol path,
especially `specs/ARCH-client-runtime.md` and
`../rostra-core/specs/SPEC-event-graph.md`.

Keep RPC verification, synchronization admission, database mutation ordering,
and their adversarial tests synchronized with the security boundary described
there.
