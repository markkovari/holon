//! Part 2 — payments, shipments, returns. UNIMPLEMENTED: see CONTRACT.md.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "orders", _, "pay"]) => {
            Reply::err(501, "not_implemented: buyer pays, fires payment+order machines")
        }
        (Method::Post, ["api", "orders", _, "ship"]) => {
            Reply::err(501, "not_implemented: admin/vendor ships, fires shipment+order machines")
        }
        (Method::Post, ["api", "shipments", _, "deliver"]) => {
            Reply::err(501, "not_implemented: admin delivers, fires shipment THEN order machine")
        }
        (Method::Post, ["api", "orders", _, "refund"]) => {
            Reply::err(501, "not_implemented: admin refunds (full or partial), posts ledger entry")
        }
        (Method::Post, ["api", "orders", _, "returns"]) => {
            Reply::err(501, "not_implemented: buyer requests a return once delivered")
        }
        (Method::Post, ["api", "returns", _, "approve"]) => {
            Reply::err(501, "not_implemented: admin approves a return")
        }
        (Method::Post, ["api", "returns", _, "reject"]) => {
            Reply::err(501, "not_implemented: admin rejects a return")
        }
        _ => Reply::err(404, "not_found"),
    }
}
