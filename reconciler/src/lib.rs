//! The reconciler, as a library so `comp app plan` runs the same diff the loop
//! runs. That is the whole reason this crate is `[lib]` + `[[bin]]`: a dry run that
//! is only *nearly* the real logic is worse than no dry run at all.

pub mod bucket;
pub mod budget;
/// "Do we already have something for this?", asked of the catalogue (ADR-0089).
pub mod capsearch;
pub mod catalogue;
/// Joining the parts of a decomposed goal: mocks, the merge, the composition gate.
pub mod compose;
/// The interface two parts of a decomposed goal build against (ADR-0086).
pub mod contract;
pub mod cost;
/// Starting a fleet, driving it, reading it.
///
/// A library module rather than test-only code because the benchmark matrix needs
/// exactly what the tests need — and two harnesses that start fleets slightly
/// differently would produce numbers that cannot be compared with the assertions.
pub mod fleet;
pub mod generation;
/// Skipping work already done, and recording every verdict so the next run can.
pub mod memory;
pub mod money;
pub mod oci;
pub mod plan;
/// Composition as a library call — wrap `wac`, do not run it.
pub mod plug;
pub mod router;
pub mod settings;
pub mod spec;
/// What a run leaves behind (ADR-0092).
pub mod trace;
pub mod wallet;
