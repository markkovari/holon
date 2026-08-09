//! The reconciler, as a library so `comp app plan` runs the same diff the loop
//! runs. That is the whole reason this crate is `[lib]` + `[[bin]]`: a dry run that
//! is only *nearly* the real logic is worse than no dry run at all.

pub mod oci;
pub mod plan;
pub mod settings;
pub mod spec;
