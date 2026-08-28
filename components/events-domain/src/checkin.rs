//! `checkin` — NOT IMPLEMENTED. This file is one part's whole goal.
//!
//! See CONTRACT.md for the routes, the status codes and the stored shapes. Every
//! route answers 501 until it is written, which is what lets the other three parts
//! be judged while this one is still a stub.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
