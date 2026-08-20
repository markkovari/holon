//! `verdict` — the model's opinion, and what the policy does to it.
//!
//! NOT IMPLEMENTED. This is your file, and precedence is the whole of it: a rule that
//! matched decides, and the model decides only when no rule did. A decision that does
//! not record both cannot be audited.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
