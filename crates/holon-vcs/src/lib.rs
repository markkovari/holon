//! `holon-vcs` — an agent-native code store (ADR-0099).
//!
//! An operation log over a commutative, symbol-level patch graph, in place of git's
//! snapshots and line diffs. Every edit is an automatic, anonymous commit; edits to
//! different symbols commute; edits to the same symbol from the same parent become a
//! first-class *conflict* record rather than a failure.
//!
//! The WIT contract is `wit/vcs/vcs.wit` (`holon:vcs/code-store`); [`model`] mirrors
//! its types one to one and [`engine::Engine`] implements its operations:
//!
//! | module  | what | target |
//! |---|---|---|
//! | [`model`]  | the WIT records, variants and enums | both |
//! | [`store`]  | content-addressed blobs and CAS pointers, as traits | both |
//! | [`graph`]  | symbols, patches, conflicts and their edges, as a trait | both |
//! | [`oplog`]  | the linear, undoable operation log of write-ahead intents | both |
//! | [`order`]  | order keys: where a symbol sits in its file | both |
//! | [`patch`]  | patch hashes and the applied/duplicate/conflicted decision | both |
//! | [`engine`] | the contract's operations over the four traits | both |
//! | [`recovery`] | the write order, settling half-done ops, verify and repair | both |
//! | [`git`]    | git blob and tree ids for a snapshot | both |
//! | [`mem`]    | in-memory backends | both |
//! | [`extract`] | Rust/WIT files split into symbols losslessly; edited files ingested as patches | both |
//! | [`wire`]   | the JSON `comp-vcs` serves and its components send | both |
//! | `nats`, `surreal` | JetStream ObjectStore/KV and SurrealDB adapters | `native` only |
//!
//! The precise semantics — what `commuted` means, how an N-way race is recorded,
//! what `revert-op` refuses, how names and positions work — are in [`engine`]'s
//! and [`patch`]'s module docs; why a crash at any point is recoverable is in
//! [`recovery`]'s.

pub mod engine;
pub mod error;
pub mod extract;
pub mod git;
pub mod graph;
pub mod mem;
pub mod model;
pub mod oplog;
pub mod order;
pub mod patch;
pub mod recovery;
pub mod store;
pub mod wire;

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
