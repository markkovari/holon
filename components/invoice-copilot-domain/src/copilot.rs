//! `copilot` — the model names the lines; it does not do the arithmetic.
//!
//! NOT IMPLEMENTED. This is your file. A model asked to divide 100.00 three ways answers
//! 33.33 three times and is confident about it. `money::allocate` is the only thing here
//! allowed to produce a number.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
