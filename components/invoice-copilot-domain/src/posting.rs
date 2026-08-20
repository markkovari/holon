//! `posting` — the only irreversible step, and it happens once.
//!
//! NOT IMPLEMENTED. This is your file. Every HTTP client retries; a posting route without
//! an idempotency key is a double-charge waiting for one.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
