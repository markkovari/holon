//! `reconcile` — see CONTRACT.md.
//!
//! NOT IMPLEMENTED. This is your file, and the properties it has to satisfy are about many
//! requests at once rather than about any one of them. Read "The one thing this whole app is
//! about" in the contract before writing a line.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
