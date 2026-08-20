//! `invoices` — what currency the arithmetic will be done in.
//!
//! NOT IMPLEMENTED. This is your file. `CONTRACT.md` says what these routes answer. A
//! currency nobody can add up is an invoice that cannot be totalled, and finding that out
//! at posting time is finding it out too late.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
