//! `reply` — the model drafts it, and the outbox owns it.
//!
//! NOT IMPLEMENTED. This is your file. You must not send anything: a drafted reply is
//! ENQUEUED, and the gate asserts this artifact does not even import the sender. 202, not
//! 200 — nothing has been delivered when you answer.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
