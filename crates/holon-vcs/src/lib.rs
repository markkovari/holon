//! `holon-vcs` — an agent-native code store (ADR-0099).
//!
//! An operation log over a commutative, symbol-level patch graph, in place of git's
//! snapshots and line diffs. Every edit is an automatic, anonymous commit; edits to
//! different symbols commute; edits to the same symbol from the same parent become a
//! first-class *conflict* record rather than a failure.
//!
//! The WIT contract is `wit/vcs/vcs.wit` (`holon:vcs/code-store`); [`model`] mirrors
//! its types one to one and [`engine::Engine`] implements its seven operations:
//!
//! | module  | what | target |
//! |---|---|---|
//! | [`model`]  | the WIT records, variants and enums | both |
//! | [`store`]  | content-addressed blobs and CAS pointers, as traits | both |
//! | [`graph`]  | symbols, patches, conflicts and their edges, as a trait | both |
//! | [`oplog`]  | the linear, undoable operation log | both |
//! | [`patch`]  | patch hashes and the applied/duplicate/conflicted decision | both |
//! | [`engine`] | the seven operations over the four traits | both |
//! | [`git`]    | git blob and tree ids for a snapshot | both |
//! | [`mem`]    | in-memory backends | both |
//! | `nats`, `surreal` | JetStream ObjectStore/KV and SurrealDB adapters | `native` only |
//!
//! The precise semantics — what `commuted` means, how an N-way race is recorded,
//! what `revert-op` refuses — are in [`engine`]'s and [`patch`]'s module docs.

pub mod engine;
pub mod error;
pub mod git;
pub mod graph;
pub mod mem;
pub mod model;
pub mod oplog;
pub mod patch;
pub mod store;

#[cfg(feature = "native")]
pub mod nats;
#[cfg(feature = "native")]
pub mod surreal;

pub use engine::Engine;
pub use error::VcsError;

/// An engine entirely in memory.
pub type MemEngine = Engine<mem::MemBlobs, mem::MemPointers, mem::MemGraph, mem::MemOpLog>;

/// A fresh in-memory engine.
pub fn mem_engine() -> MemEngine {
    Engine::new(mem::MemBlobs::new(), mem::pointers(), mem::MemGraph::new(), mem::oplog())
}
