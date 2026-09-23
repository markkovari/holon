//! See CONTRACT.md "Game routes" — this module owns the routes listed there as `curation.rs`.
//! Scaffold: every route answers 501 until it is built.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let _ = (method, route, body);
    Reply::err(501, "not_implemented")
}
