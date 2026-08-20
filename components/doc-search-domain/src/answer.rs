//! `answer` — the only part that spends anything.
//!
//! NOT IMPLEMENTED. This is your file, and the order of the checks IS the
//! specification: step-up, then cache, then retrieval, then budget, then the model.
//! Each one exists to stop the cost of the next. A second identical question must cost
//! nothing at all.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
