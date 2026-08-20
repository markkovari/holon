//! `courier` — the only part that talks to the far end.
//!
//! NOT IMPLEMENTED. This is your file, and what happens when a send FAILS is the whole of
//! it. An ack is a claim that the reply arrived; a fail is a claim that it did not. Getting
//! either wrong loses a customer's reply or sends it twice, and neither shows up in a
//! request that succeeds.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
