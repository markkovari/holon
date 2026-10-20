//! The goal loop's library; depends on `comp-fleet` and nothing of the control plane (ADR-0115).

pub mod capsearch;
pub mod catalogue;
pub mod compose;
pub mod contract;
pub mod gate;
pub mod generation;
pub mod goalbatch;
pub mod goalexit;
pub mod memory;
pub mod trace;
