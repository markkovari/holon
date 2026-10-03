//! `desktop-notifier-domain` — raise a desktop notification from an HTTP call

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/ratelimit-guard",
            "../audit-log/wit",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../ui-notifier/wit",
            "wit",
        ],
        world: "local:desktop-notifier/domain",
        generate_all,
    });
    /// Stable names for the p3 `wasi:http` (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
use bindings::p3::handler::Guest;
use bindings::os::ui::notifications;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::wasi::keyvalue::store;
use serde_json::json;

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Html(200, r#"<!DOCTYPE html><html><head><title>Notification Center</title></head><body><h1>Notification Center</h1><button onclick="fetch('/api/notify').then(r=>r.json()).then(d=>document.getElementById('r').innerText=JSON.stringify(d))">Send Notification</button><div id="r"></div></body></html>"#.to_string()),
            (Method::Get, ["api", "notify"]) => match notifications::notify("Hello from WASM!") {
                Ok(()) => Outcome::Json(200, json!({ "status": "sent" }).to_string()),
                Err(notifications::NotifyError::Unavailable(d)) => Outcome::Err(503, d),
            },
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(outcome)
    }
}

enum Outcome {
    Html(u16, String),
    Json(u16, String),
    Err(u16, String),
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    let (code, body, content_type) = match result {
        Outcome::Html(c, b) => (c, b, b"text/html".to_vec()),
        Outcome::Json(c, b) => (c, b, b"application/json".to_vec()),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string(), b"application/json".to_vec()),
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type]);
    respond_with(code, headers, body.into_bytes())
}
bindings::export!(Component with_types_in bindings);

guestio::guest_p3_respond!();
