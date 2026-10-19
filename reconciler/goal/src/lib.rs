//! The goal loop's library: what a run does with a goal — generate candidates, join the parts of a
//! decomposed goal, judge them through a gate, remember what each attempt taught, leave a trace — and
//! the catalogue it asks "do we already have something for this?" (ADR-0081 to ADR-0092).
//!
//! Library for the tests, which exercise each of these directly, and for `comp-goalrun` and
//! `comp-goald`, which drive them. It depends on `comp-fleet` for the fleet it starts and the
//! composition it needs, and on nothing of the control plane (docs/adr/0115).

/// "Do we already have something for this?", asked of the catalogue (ADR-0089).
pub mod capsearch;
pub mod catalogue;
/// Joining the parts of a decomposed goal: mocks, the merge, the composition gate.
pub mod compose;
/// The interface two parts of a decomposed goal build against (ADR-0086).
pub mod contract;
pub mod gate;
pub mod generation;
/// Which queued goals one batch-start starts, and in what order — the pure decision behind
/// `holon goal start --queued <project>` (ADR-0082).
pub mod goalbatch;
/// What `comp-goalrun`'s exit code means, and how `comp-goald` reads it.
pub mod goalexit;
/// Skipping work already done, and recording every verdict so the next run can.
pub mod memory;
/// What a run leaves behind (ADR-0092).
pub mod trace;
