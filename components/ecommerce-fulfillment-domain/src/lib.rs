//! E-commerce fulfillment flow handling orders

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../host/wit/deps/comp-store",
            "../../wit/deps/ratelimit-guard",
            "../audit-log/wit",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../record-store/wit",
            "../ledger/wit",
            "../fsm-workflow/wit",
            "../stripe-gateway/wit",
            "wit",
        ],
        world: "domain:ecommerce/ecommerce-fulfillment",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod handlers;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use serde_json::{json, Value};

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

pub struct Reply {
    pub status: u16,
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    pub bearer: String,
    pub idempotency_key: String,
}

use guestfmt::percent_decode as percent;

fn header(request: &Request, name: &str) -> String {
    let fields = request.get_headers();
    let values = fields.get(name);
    values.first().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
}

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let bearer_val = bearer(&request).unwrap_or_default();
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer: bearer_val,
            idempotency_key: header(&request, "idempotency-key"),
        };
        let method = request.get_method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(request).await,
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();

        // Static UI Serving
        if seg.is_empty() || seg.as_slice() == ["index.html"] {
            return respond(200, "text/html", include_str!("../ui/index.html"));
        }
        if seg.as_slice() == ["style.css"] {
            return respond(200, "text/css", include_str!("../ui/style.css"));
        }
        if seg.as_slice() == ["app.js"] {
            return respond(200, "application/javascript", include_str!("../ui/app.js"));
        }

        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["register"] => handlers::register(&method, &body),
            ["login"] => handlers::login(&method, &body),
            ["api", "orders"] => handlers::orders(&method, &route, &body),
            ["api", "orders", id, "fulfill"] => handlers::fulfill_order(&method, &route, id, &body),
            _ => Reply::err(404, &format!("not_found: {:?}", seg)),
        };

        let body = if payload.is_null() { String::new() } else { payload.to_string() };
        respond(status, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
