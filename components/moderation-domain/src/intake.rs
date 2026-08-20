//! `intake` — what gets into the queue at all.
//!
//! NOT IMPLEMENTED. This is your file. `CONTRACT.md` says what these routes answer.
//! The limiter is a component: a counter of your own in the record store answers 429
//! eventually and fails this part's gate.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
