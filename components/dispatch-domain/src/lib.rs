//! `dispatch-domain` — service requests, scheduling and the day's manifest.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../host/wit/deps/comp-store",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../record-store/wit",
            "../id-generate/wit",
            "../pii-redact/wit",
            "../fsm-workflow/wit",
            "../geo/wit",
            "../csv/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "dispatch:domain/dispatch-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod manifest;
mod requests;
mod schedule;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::records::store::store as records;
use serde_json::{json, Value};

guestio::guest_p3_respond!();

struct Component;

pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all.
    pub json: Value,
    /// Not serialised as JSON: a `Value::String` would become one quoted blob.
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

/// Written straight to the store so a part is judged alone. The third is pre-assigned so `manifest`
/// has one without `schedule`; the first title's comma forces CSV quoting.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (title, notes, lat, lon, state, engineer, distance_m) in [
        ("Boiler leaking, badly", "caller is on 555-0143", 47.4790, 19.0600, "new", "", 0),
        ("Lift stuck between floors", "reported by the concierge", 47.5300, 19.0440, "new", "", 0),
        ("Meter reads zero", "access code is on file", 47.4750, 19.0600, "assigned", "cili", 557),
    ] {
        match records::create(
            "requests",
            &json!({
                "title": title,
                "notes": notes,
                "lat": lat,
                "lon": lon,
                "state": state,
                "engineer": engineer,
                "distance_m": distance_m,
                "created": "2026-09-02T09:00:00Z",
            })
            .to_string(),
            &["state".to_string(), "engineer".to_string()],
        ) {
            Ok(e) => ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    Reply::json(201, json!({ "request_ids": ids }))
}

/// Past this the body reads as empty, rather than the store's memory cap trapping the component.
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
            // Scaffold: `schedule`'s gate sees what it wrote without the `requests` read route.
            ["test", "request", id] => match records::get("requests", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // The CSV arm must precede a bare ["manifest"] match.
            ["api", "manifest.csv"] | ["api", "manifest"] => {
                manifest::handle(&method, &route, &body)
            }
            // Before the `api/requests` arm: these belong to `schedule`.
            ["api", "requests", _, "assign"]
            | ["api", "requests", _, "transition"]
            | ["api", "queue"] => schedule::handle(&method, &route, &body),
            ["api", "requests", ..] => requests::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        match raw {
            Some((ct, bytes)) => respond(status, &ct, bytes),
            None if !payload.is_null() => respond(status, "application/json", payload.to_string()),
            None => respond(status, "application/json", Vec::new()),
        }
    }
}

bindings::export!(Component with_types_in bindings);
