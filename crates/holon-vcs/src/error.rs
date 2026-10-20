//! The errors `holon:vcs/types.vcs-error` names, one to one.

use thiserror::Error;

use crate::model::{CasFailure, SymbolId};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsError {
    #[error("storage: {0}")]
    Storage(String),
    /// A race on our own write, not two agents disagreeing (that is a conflict).
    #[error("concurrent modification of {}: expected {:?}, found {:?}", .0.pointer, .0.expected, .0.actual)]
    ConcurrentModification(CasFailure),
    #[error("unresolved conflicts: {0:?}")]
    UnresolvedConflict(Vec<String>),
    #[error("symbol not found: {0}")]
    SymbolNotFound(SymbolId),
    /// Two different symbols wanting one name, not a conflict between two versions of one.
    #[error("name taken: {0}")]
    NameTaken(SymbolId),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

impl VcsError {
    pub fn storage(e: impl std::fmt::Display) -> Self {
        VcsError::Storage(e.to_string())
    }
}

pub type Result<T, E = VcsError> = std::result::Result<T, E>;
