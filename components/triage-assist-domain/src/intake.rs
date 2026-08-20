//! `intake` — taking a defect report in, authenticated, throttled, and masked.
//!
//! NOT IMPLEMENTED. This is your file: `CONTRACT.md` says what these routes answer,
//! and `wit/assist.wit` says which capabilities are already in your world. The three
//! that make this part what it is — `auth:identity/authorizer`,
//! `ratelimit:guard/limiter` and `pii:redact/redactor` — are implemented, tested and
//! composed for you. You call them; you do not write them.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
