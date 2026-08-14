//! The reconciler, as a library so `comp app plan` runs the same diff the loop
//! runs. That is the whole reason this crate is `[lib]` + `[[bin]]`: a dry run that
//! is only *nearly* the real logic is worse than no dry run at all.

/// Starting a fleet, driving it, reading it.
///
/// A library module rather than test-only code because the benchmark matrix needs
/// exactly what the tests need — and two harnesses that start fleets slightly
/// differently would produce numbers that cannot be compared with the assertions.
pub mod fleet;
pub mod budget;
pub mod wallet;
pub mod cost;
pub mod generation;
pub mod oci;
pub mod plan;
pub mod settings;
pub mod spec;
