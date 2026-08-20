//! `ledger` — what happened, durably and queryably.
//!
//! NOT IMPLEMENTED. This is your file, and `note` is the protocol: the router and
//! both other parts call it, so its signature is not yours to change — only its
//! body. `audit:log/recorder` is where an event goes and `audit:log/query` is how it
//! comes back.
//!
//! `note` returning nothing is deliberate. An audit backend that is down is a `note`
//! that did nothing, never a 500 on somebody else's report.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn note(_trace: &str, _event: &str, _outcome: &str, _subject: &str, _detail: &str) {}

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
