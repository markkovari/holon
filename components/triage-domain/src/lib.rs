//! `triage-domain` — defect intake, lifecycle and digest, as one component.
//! own storage shape passes its own gate and fails the composition.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../id-generate/wit",
            "../pii-redact/wit",
            "../fsm-workflow/wit",
            "../csv/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "triage:domain/triage-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod digest;
mod intake;
mod workflow;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::records::store::store as records;
use serde_json::{json, Value};

guestio::guest_p3_respond!();

struct Component;

pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all.
    pub json: Value,
    /// `text/csv` can't be a `Value`: `Value::String` serialises to one quoted JSON string.
    pub raw: Option<(String, Vec<u8>)>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body, raw: None }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// A JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
    pub fn raw(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
        Reply { status, json: Value::Null, raw: Some((content_type.to_string(), bytes)) }
    }
}

/// No bearer: this API has no auth, deliberately.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
}

impl Route {
    pub fn param(&self, key: &str) -> String {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| percent(v))
            .unwrap_or_default()
    }
}

use guestfmt::percent_decode as percent;

/// Lets `workflow` and `digest` be judged before `intake` exists; `severity` is `workflow`'s job.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (title, body, component) in [
        // A comma in the title: `digest`'s CSV has to quote it or the row loses a column.
        ("Login fails, silently", "no error shown to the user", "auth"),
        ("Checkout total is wrong", "off by one cent on 3 items", "billing"),
    ] {
        match records::create(
            "reports",
            &json!({
                "title": title,
                "body": body,
                "component": component,
                "state": "open",
                "reported_at": "2026-08-17T09:00:00Z",
            })
            .to_string(),
            &["component".to_string(), "state".to_string()],
        ) {
            Ok(e) => ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    Reply::json(201, json!({ "report_ids": ids }))
}

/// Past this the body reads as empty, instead of growing until the memory cap traps the component.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
        };
        let method = request.get_method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(request).await,
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload, raw } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "seed"] => seed(),
            // Straight out of the store, so a part is judged on what it WROTE.
            ["test", "report", id] => match records::get("reports", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // The CSV arm must come first, or a match on ["digest"] alone would swallow it.
            ["api", "digest.csv"] | ["api", "digest"] => digest::handle(&method, &route, &body),
            // Before `api/reports`, which would hand `/transition` to `intake`.
            ["api", "reports", _, "transition"] | ["api", "queue"] => {
                workflow::handle(&method, &route, &body)
            }
            ["api", "reports", ..] => intake::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let content_type = match &raw {
            Some((ct, _)) => ct.as_str(),
            None => "application/json",
        };
        let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
        let bytes = match raw {
            Some((_, bytes)) => bytes,
            None if !payload.is_null() => payload.to_string().into_bytes(),
            None => Vec::new(),
        };
        respond_with(status, headers, bytes)
    }
}

bindings::export!(Component with_types_in bindings);
