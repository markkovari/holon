//! Part 4 — the double-entry payout ledger. UNIMPLEMENTED: see CONTRACT.md.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Get, ["api", "vendors", _, "balance"]) => {
            Reply::err(501, "not_implemented: trial-balance net for vendor:<subject>")
        }
        (Method::Get, ["api", "ledger", "platform"]) => {
            Reply::err(501, "not_implemented: admin-only platform cash/fees balance")
        }
        (Method::Post, ["api", "vendors", _, "payout"]) => {
            Reply::err(501, "not_implemented: admin zeroes a vendor's balance")
        }
        _ => Reply::err(404, "not_found"),
    }
}
