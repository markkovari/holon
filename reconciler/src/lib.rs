//! The reconciler, as a library so `comp app plan` runs the same diff the loop
//! runs. That is the whole reason this crate is `[lib]` + `[[bin]]`: a dry run that
//! is only *nearly* the real logic is worse than no dry run at all.

/// Starting a fleet, driving it, reading it — and composition as a library call. They live in
/// `comp-fleet` (docs/adr/0115) so that a daemon's own end-to-end test can start a fleet without
/// depending on the control plane; the paths everything already used are re-exported here.
pub use comp_fleet::{fleet, plug};

/// Artifacts by digest. Their own package (docs/adr/0115), so that the tools that name an artifact need
/// not depend on the control plane; `comp_reconciler::oci` is the crate, re-exported.
pub use comp_oci as oci;

pub mod plan;
pub mod settings;
pub mod spec;
