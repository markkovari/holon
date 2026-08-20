//! `queue` — the rules, what is waiting, and what has left.
//!
//! NOT IMPLEMENTED. This is your file. The rules live in `policy:guard` and are read
//! back from it: a copy of your own is how the rules a reviewer reads stop being the
//! rules their decisions used.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
