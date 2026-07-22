//! Model-based schedule properties for durable client-database semantics.
//!
//! These tests deliberately compare semantic, typed snapshots rather than raw
//! database state. Local receipt order, request clocks, watch scheduling,
//! session authority, physical GC residue, and absent-versus-zero aggregate
//! rows are not convergence oracles.
//!
//! See `docs/property-testing.md` for the modeled Linked Specs invariants,
//! exclusions, runtime budget, and soak command.

mod content;
mod graph;
mod reducers;
mod runner;
