//! `library` — what can be found, and how a question finds it.
//!
//! NOT IMPLEMENTED. This is your file. `CONTRACT.md` says what these routes answer.
//! `search:index` is the index — storing a document without indexing it, or matching
//! titles by hand instead of querying, both answer correctly on one happy path and
//! fail this part's gate.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
