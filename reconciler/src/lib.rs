//! The reconciler, as a library so `comp app plan` runs the same diff the loop
//! runs. That is the whole reason this crate is `[lib]` + `[[bin]]`: a dry run that
//! is only *nearly* the real logic is worse than no dry run at all.

/// Artifacts by digest. Their own package (docs/adr/0115), so that the tools that name an artifact need
/// not depend on the control plane; `comp_reconciler::oci` is the crate, re-exported.
pub use comp_oci as oci;

pub mod plan;
pub mod settings;
pub mod spec;
