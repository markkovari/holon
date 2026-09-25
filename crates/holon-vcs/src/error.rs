//! The errors `holon:vcs/types.vcs-error` names, one to one.

use thiserror::Error;

use crate::model::{CasFailure, SymbolId};

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
    SymbolNotFound(SymbolId),
    /// A `create` or `rename` wanted a name another live symbol holds (or won a
    /// race for). Nothing was written. Distinct from a conflict: the two are
    /// different symbols, not two versions of one.
    #[error("name taken: {0}")]
    NameTaken(SymbolId),
    /// A patch, op or conflict id that does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A malformed request.
    #[error("invalid: {0}")]
    Invalid(String),
}

impl VcsError {
    pub fn storage(e: impl std::fmt::Display) -> Self {
        VcsError::Storage(e.to_string())
    }
}

pub type Result<T, E = VcsError> = std::result::Result<T, E>;
