//! What the tests, the benchmarks and the goal loop share to stand a platform up: a fleet of hosts on a
//! NATS lattice to drive and read ([`fleet`]), and the composition it needs, as a library call ([`plug`]).
//!
//! Library, not test-only. The benchmark matrix needs the same fleet start as the tests, so its numbers
//! stay comparable, and the daemons' own end-to-end tests start from here too (docs/adr/0115).
//! `comp_reconciler::fleet` and `comp_reconciler::plug` are this crate's modules, re-exported, so
//! nothing that named them there changed.

/// Starting a fleet, driving it, reading it.
pub mod fleet;
/// Composition as a library call — wrap `wac`, do not run it.
pub mod plug;
