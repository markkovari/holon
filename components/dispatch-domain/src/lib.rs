//! `dispatch-domain` — service requests, scheduling and the day's manifest.
//!
//! This file is the ROUTER and no part may write it: it dispatches to `requests`, `schedule`
//! and `manifest`, answers `/health` so the harness can tell "not up" from "wrong", and
//! seeds a fixture. `src/requests.rs`, `src/schedule.rs` and `src/manifest.rs` are the goal;
//! `CONTRACT.md` is what they must agree on.
//!
//! Next to `triage-domain` because there each part imported a capability the others did not;
//! here `geo:resolve` is imported by TWO parts (`schedule` picks the nearest engineer,
//! `manifest` filters by radius), so a part that hand-rolls haversine can pass its own gate
//! and disagree with its sibling. Only the composition sees it.

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
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
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

/// What a handler answers with: a status, and a body that is JSON unless the
/// handler says otherwise.
pub struct Reply {
    pub status: u16,
    /// The JSON body. `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
    /// A body the router must NOT serialise as JSON, with its content type: `Value::String`
    /// serialises to a JSON string *literal*, so a CSV parser would read one quoted blob.
    pub raw: Option<(String, Vec<u8>)>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body, raw: None }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// 204 carries no body, and a JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
    /// A body sent through byte-for-byte, under the content type you name.
    pub fn raw(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
        Reply { status, json: Value::Null, raw: Some((content_type.to_string(), bytes)) }
    }
}

/// The path segments of a request and its query string. No bearer: this API has no auth,
/// deliberately (the clinic already proves a part can call `auth:identity`).
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

/// Three requests written straight to the store, so a part can be judged before the part
/// upstream exists (all three are written concurrently by different agents, so the fixture
/// writes the contract's document shape directly).
///
///   * the first two are `new` with no engineer: assigning is `schedule`'s job, and
///     pre-filling would let a part that never assigns pass its own gate;
///   * the THIRD is pre-assigned because `manifest` must report `by_engineer` and
///     `total_distance_m`, and the only other way to get an assigned request is `schedule`'s
///     route, a stub while `manifest` is judged alone. Its `distance_m` is a fixed number the
///     gate only SUMS.
///
/// The first title carries a comma so `manifest`'s CSV has to quote it.
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

/// A ceiling on a body read into memory, not a policy: past this the read gives up
/// and the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
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

        // The router: `/health` and the fixture here, everything else to the part
        // that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload, raw } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "seed"] => seed(),
            // The stored document, straight out of the store. Scaffold: `schedule`'s gate
            // must see what it WROTE without depending on `requests`' `GET /api/requests/{id}`,
            // a stub that would make the gate blame `schedule`.
            ["test", "request", id] => match records::get("requests", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // `manifest.csv` and `manifest` both start with "manifest", and the CSV
            // arm must come first or a path-segment match on ["manifest"] alone
            // would swallow it.
            ["api", "manifest.csv"] | ["api", "manifest"] => {
                manifest::handle(&method, &route, &body)
            }
            // Before the `api/requests` arm: these belong to `schedule`, and a match
            // on ["api","requests",..] would hand them to `requests` instead.
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
