//! The reconciler, as a library so `comp app plan` runs the same diff the loop
//! runs. That is the whole reason this crate is `[lib]` + `[[bin]]`: a dry run that
//! is only *nearly* the real logic is worse than no dry run at all.

/// Starting a fleet, driving it, reading it — and composition as a library call. They live in
/// `comp-fleet` (docs/adr/0115) so that a daemon's own end-to-end test can start a fleet without
/// depending on the control plane; the paths everything already used are re-exported here.
pub use comp_fleet::{fleet, plug};

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
pub mod gate;
pub mod generation;
/// Which queued goals one batch-start starts, and in what order — the pure
/// decision behind `holon goal start --queued <project>` (ADR-0082).
pub mod goalbatch;
/// What `comp-goalrun`'s exit code means, and how `comp-goald` reads it.
pub mod goalexit;
/// Skipping work already done, and recording every verdict so the next run can.
pub mod memory;
pub mod money;
pub mod oci;
pub mod offpeak;
pub mod plan;
pub mod router;
pub mod settings;
pub mod spec;
/// What a run leaves behind (ADR-0092).
pub mod trace;
pub mod wallet;
