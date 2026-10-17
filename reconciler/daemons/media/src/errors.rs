//! `refused` is a decision, `not-found` a missing thing, `unavailable` the
//! store or the queue — the contract's error shape. A caller should retry
//! only the last.

use axum::Json;
use serde_json::{json, Value};

#[derive(Debug)]
pub(crate) enum MediaError {
    Refused(String),
    NotFound(String),
    Unavailable(String),
}

impl MediaError {
    pub(crate) fn json(&self) -> Json<Value> {
        let (kind, detail) = match self {
            MediaError::Refused(d) => ("refused", d),
            MediaError::NotFound(d) => ("not-found", d),
            MediaError::Unavailable(d) => ("unavailable", d),
        };
        Json(json!({ "error": kind, "detail": detail }))
    }
}

pub(crate) fn unavailable(e: impl std::fmt::Display) -> MediaError {
    MediaError::Unavailable(e.to_string())
}

/// Every route answers 200: a non-200 means transport, same as `comp-imageopt`.
pub(crate) fn answer(r: Result<Value, MediaError>) -> Json<Value> {
    match r {
        Ok(v) => Json(v),
        Err(e) => e.json(),
    }
}
