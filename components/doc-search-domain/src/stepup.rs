//! `stepup` — the second factor, and the mark the `answer` part reads.
//!
//! NOT IMPLEMENTED. This is your file. `otp:totp` provisions the secret and checks the
//! code; a hand-rolled HMAC here fails the gate however correct it is. The state you
//! write is read by another part, so its shape is the contract's, not yours.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
