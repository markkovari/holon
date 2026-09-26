//! The errors `holon:park/types.park-error` names, one to one.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParkError {
    /// A bad ticket id, or a `wake` whose `correlation` nothing parked has.
    #[error("not found: {0}")]
    NotFound(String),
    /// `take-ready` or `cancel` on a ticket already `resumed` or `cancelled`.
    #[error("already closed: {0}")]
    AlreadyClosed(String),
    /// A backing store was unreachable or refused. Retrying may help.
    #[error("storage: {0}")]
    Storage(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

impl ParkError {
    pub fn storage(e: impl std::fmt::Display) -> Self {
        ParkError::Storage(e.to_string())
    }
}

pub type Result<T, E = ParkError> = std::result::Result<T, E>;
