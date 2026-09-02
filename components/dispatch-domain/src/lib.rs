//! `dispatch-domain` — service requests, scheduling and the day's manifest.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: it dispatches to `requests`,
//! `schedule` and `manifest`, answers `/health` so the harness can tell "the
//! component is not up" from "the component is wrong", and seeds a fixture. Three
//! parts need it and none owns it, which is the shape a shared file has to have
//! when three agents work at once — the alternative is a merge conflict on every
//! run.
//!
//! `src/requests.rs`, `src/schedule.rs` and `src/manifest.rs` are the goal.
//! `CONTRACT.md` is what they must agree on.
//!
//! ## Why this goal exists next to `triage-domain`
//!
//! Same three-part shape, one deliberate difference. In triage each part imported a
//! capability the others did not, so a part that reimplemented one failed alone, in
//! its own gate. Here `geo:resolve` is imported by TWO parts — `schedule` picks the
//! nearest engineer with it, `manifest` filters by radius with it — so a part that
//! hand-rolls haversine can be internally consistent, pass its own gate, and
//! disagree with its sibling. Only the composition sees it.

#[allow(warnings)]
mod bindings;
mod manifest;
mod requests;
mod schedule;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::records::store::store as records;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();

struct Component;

/// What a handler answers with: a status, and a body that is JSON unless the
/// handler says otherwise.
pub struct Reply {
    pub status: u16,
    /// The JSON body. `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
    /// A body the router must NOT serialise as JSON, with its content type.
    ///
    /// `text/csv` cannot be expressed as a `Value`: `Value::String` serialises to a
    /// JSON string *literal*, surrounding quotes and `\"` escapes included, so a
    /// CSV parser reads one quoted blob instead of columns.
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

/// The path segments of a request and its query string.
///
/// No bearer here: this API has no auth, deliberately. The clinic already proves a
/// part can be made to call `auth:identity` rather than hash a password, and
/// repeating it would spend a part's whole budget on a lesson already recorded.
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

/// Three requests written straight to the store, so a part can be judged before the
/// part upstream of it exists.
///
/// This is the three-part problem in one function. `schedule` is judged on moving a
/// request through its lifecycle and `manifest` on counting what the other two
/// produced — both need requests, and neither may depend on `requests` being
/// finished, because all three are written at the same time by different agents. So
/// the fixture writes the contract's document shape directly.
///
/// Two decisions in here, and both are scaffold saying what it is:
///
///   * the first two are `new` with no engineer, because assigning is `schedule`'s
///     job and a fixture that pre-filled every one would let a part that never
///     assigns anything pass its own gate;
///   * the THIRD is pre-assigned, because `manifest` has to report `by_engineer` and
///     `total_distance_m`, and the only other way to get an assigned request is
///     `POST /api/requests/{id}/assign` — which belongs to `schedule`, is a stub
///     while `manifest` is judged alone, and would have the gate blame `manifest`
///     for a route it does not own. Its `distance_m` is a fixed number the gate
///     treats as opaque and only ever SUMS — asserting it is geometrically right
///     would be this fixture testing `geo`, which has its own tests.
///
/// The first title carries a comma, for the same reason triage's does: `manifest`'s
/// CSV has to quote it or the row loses a column.
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

guestio::guest_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        // The router: `/health` and the fixture here, everything else to the part
        // that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload, raw } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "seed"] => seed(),
            // The stored document, straight out of the store. Scaffold, and it says
            // what it is: a part must be judgeable on what it WROTE without
            // depending on the part that owns the route for reading it back.
            //
            // `schedule`'s gate needs exactly this. The contract makes it update the
            // request document as well as the fsm instance, and the only other way
            // to see the document is `GET /api/requests/{id}` — which belongs to
            // `requests`, is a stub while `schedule` is judged alone, and would
            // answer `not_implemented` to a gate that then blamed `schedule`.
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

        let headers = Fields::new();
        let content_type = match &raw {
            Some((ct, _)) => ct.as_str(),
            None => "application/json",
        };
        let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            match &raw {
                Some((_, bytes)) => {
                    let _ = write_all(&stream, bytes);
                }
                None if !payload.is_null() => {
                    let _ = write_all(&stream, payload.to_string().as_bytes());
                }
                None => {}
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
