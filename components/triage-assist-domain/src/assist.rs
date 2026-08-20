//! `assist` — what the model thinks is wrong, and how badly.
//!
//! NOT IMPLEMENTED. This is your file. Read the report from the STORE, not from the
//! request: the request body of your route is empty, and the stored body is the
//! masked one. `ai:inference/inference` is your model — `classify` for the severity,
//! `summarize` for the sentence — and a provider that is down leaves the report
//! exactly as it was.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
