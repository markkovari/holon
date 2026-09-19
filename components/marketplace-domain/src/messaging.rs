//! Part 5 — buyer/vendor inquiries and threaded messages. UNIMPLEMENTED: see
//! CONTRACT.md.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "listings", _, "inquiries"]) => {
            Reply::err(501, "not_implemented: buyer opens an inquiry on a listing")
        }
        (Method::Get, ["api", "inquiries"]) => {
            Reply::err(501, "not_implemented: list inquiries for buyer/vendor/admin")
        }
        (Method::Get, ["api", "inquiries", _, "messages"]) => {
            Reply::err(501, "not_implemented: thread for either party or admin")
        }
        (Method::Post, ["api", "inquiries", _, "messages"]) => {
            Reply::err(501, "not_implemented: either party replies")
        }
        _ => Reply::err(404, "not_found"),
    }
}
