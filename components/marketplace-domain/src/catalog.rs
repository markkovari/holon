//! Part 1 — listings and order placement. UNIMPLEMENTED: see CONTRACT.md.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "listings"]) => {
            Reply::err(501, "not_implemented: create a listing, auto-labeled by Jev")
        }
        (Method::Patch, ["api", "listings", _, "category"]) => {
            Reply::err(501, "not_implemented: vendor/admin overrides the auto-label")
        }
        (Method::Get, ["api", "listings"]) => {
            Reply::err(501, "not_implemented: list active listings, optional ?category=")
        }
        (Method::Get, ["api", "listings", _]) => {
            Reply::err(501, "not_implemented: get one listing, 404 if delisted/missing")
        }
        (Method::Post, ["api", "listings", _, "delist"]) => {
            Reply::err(501, "not_implemented: vendor/admin delists a listing")
        }
        (Method::Post, ["api", "orders"]) => {
            Reply::err(501, "not_implemented: place a multi-item order, atomic stock check")
        }
        (Method::Get, ["api", "orders"]) => {
            Reply::err(501, "not_implemented: list orders, buyer sees own, admin sees all")
        }
        (Method::Get, ["api", "orders", _]) => {
            Reply::err(501, "not_implemented: get one order, buyer/admin/vendor-of-any-item only")
        }
        _ => Reply::err(404, "not_found"),
    }
}
