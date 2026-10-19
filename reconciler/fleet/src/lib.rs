//! What the tests, the benchmarks and the goal loop share to stand a platform up: a fleet of hosts on a
//! NATS lattice to drive and read ([`fleet`]), and the composition it needs, as a library call ([`plug`]).
//!
//! Library, not test-only. The benchmark matrix needs the same fleet start as the tests, so its numbers
//! stay comparable, and the daemons' own end-to-end tests start from here too (docs/adr/0115).
//! The suites, the benchmarks, the goal loop and `gpui-console` name this crate directly; the control plane
//! does not re-export it, so its own tree has no wasm toolchain in it.

/// Starting a fleet, driving it, reading it.
pub mod fleet;
/// Composition as a library call — wrap `wac`, do not run it.
pub mod plug;
