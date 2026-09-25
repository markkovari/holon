//! `holon-vcs` — an agent-native code store (ADR-0099).
//!
//! An operation log over a commutative, symbol-level patch graph, in place of git's
//! snapshots and line diffs. Every edit is an automatic, anonymous commit; edits to
//! different symbols commute; edits to the same symbol from the same parent become a
//! first-class [`error::VcsError`]-free *conflict* record rather than a failure.
//!
//! The WIT contract is `wit/vcs/vcs.wit` (`holon:vcs/code-store`). This crate is its
//! engine:
//!
//! | module  | what | target |
//! |---|---|---|
//! | [`store`]  | content-addressed blobs and CAS pointers, as traits | both |
//! | [`graph`]  | symbols, patches and their edges, as a trait | both |
//! | [`patch`]  | commutation and conflict detection | both |
//! | [`oplog`]  | the linear, undoable operation log | both |
//! | `nats`, `surreal` | the adapters behind those traits | `native` only |

pub mod error;
pub mod graph;
pub mod oplog;
pub mod patch;
pub mod store;

pub use error::VcsError;
