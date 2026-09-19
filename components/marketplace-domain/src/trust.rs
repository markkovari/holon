//! Part 3 — disputes, fraud scoring, vendor reputation. UNIMPLEMENTED: see
//! CONTRACT.md.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "orders", _, "disputes"]) => {
            Reply::err(501, "not_implemented: buyer opens a dispute, Jev gate advisory")
        }
        (Method::Get, ["api", "disputes"]) => {
            Reply::err(501, "not_implemented: list disputes, buyer sees own, admin sees all")
        }
        (Method::Post, ["api", "disputes", _, "resolve"]) => {
            Reply::err(501, "not_implemented: admin resolves, records verdict only")
        }
        (Method::Get, ["api", "vendors", _, "reputation"]) => {
            Reply::err(501, "not_implemented: dispute_rate/order_count/fraud_flags")
        }
        _ => Reply::err(404, "not_found"),
    }
}
