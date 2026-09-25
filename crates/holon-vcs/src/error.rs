//! The errors `holon:vcs/types.vcs-error` names, one to one.

use thiserror::Error;

/// A pointer and the two values a compare-and-set disagreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasFailure {
    pub pointer: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsError {
    /// A backing store was unreachable or refused. Retrying may help.
    #[error("storage: {0}")]
    Storage(String),
    /// A pointer moved between read and write; nothing changed. A race on our own
    /// write — not a disagreement between two agents' edits, which is a conflict.
    #[error("concurrent modification of {}: expected {:?}, found {:?}", .0.pointer, .0.expected, .0.actual)]
    ConcurrentModification(CasFailure),
    /// The operation needs these conflicts settled first.
    #[error("unresolved conflicts: {0:?}")]
    UnresolvedConflict(Vec<String>),
    #[error("symbol not found: {0}")]
    SymbolNotFound(String),
    /// A patch, op or conflict id that does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A malformed request.
    #[error("invalid: {0}")]
    Invalid(String),
}
