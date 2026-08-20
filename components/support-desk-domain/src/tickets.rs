//! `tickets` — what can be answered at all.
//!
//! NOT IMPLEMENTED. This is your file. `CONTRACT.md` says what these routes answer. A
//! delivery address nothing can deliver to is refused here rather than dead-lettered
//! later for a reason nobody can act on.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
