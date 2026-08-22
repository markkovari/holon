//! `triage-domain` — defect intake, lifecycle and digest, as one component.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: it dispatches to `intake`,
//! `workflow` and `digest`, answers `/health` so the harness can tell "the
//! component is not up" from "the component is wrong", and seeds a fixture. Three
//! parts need it and none owns it, which is the shape a shared file has to have
//! when three agents work at once — the alternative is a merge conflict on every
//! run.
//!
//! `src/intake.rs`, `src/workflow.rs` and `src/digest.rs` are the goal.
//! `CONTRACT.md` is what they must agree on.
//!
//! ## Why three parts and not two
//!
//! The data flows one way: `intake` writes reports, `workflow` moves them through
//! a lifecycle, `digest` counts what the other two produced. So `digest` can only
//! be right if both of the others wrote what the contract says. A part that invents
//! its own storage shape passes its own gate and fails the composition — a failure
//! two independent halves cannot produce, and the reason this exists.

#[allow(warnings)]
mod bindings;
mod digest;
mod intake;
mod workflow;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::records::store::store as records;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the cause.
/// Written the way `clinic-domain` learned to write it.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}

struct Component;

/// What a handler answers with: a status, and a body that is JSON unless the
/// handler says otherwise.
pub struct Reply {
    pub status: u16,
    /// The JSON body. `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
    /// A body the router must NOT serialise as JSON, with its content type.
    ///
    /// `text/csv` cannot be expressed as a `Value`: `Value::String` serialises to
    /// a JSON string *literal*, surrounding quotes and `\"` escapes included, so a
    /// CSV parser reads one quoted blob instead of columns. `clinic-domain` needed
    /// this arm and did not have it; here it is from the start.
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

fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Two reports written straight to the store, so a part can be judged before the
/// part upstream of it exists.
///
/// This is the three-part problem in one function. `workflow` is judged on moving a
/// report through its lifecycle and `digest` on counting reports — both need
/// reports, and neither may depend on `intake` being finished, because all three
/// are written at the same time by different agents. So the fixture writes the
/// contract's report shape directly.
///
/// Scaffold, not a feature, and it says what it is: `severity` is absent on
/// purpose, because assigning it is `workflow`'s job and a fixture that pre-filled
/// it would let a part that never assigns anything pass.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (title, body, component) in [
        // A comma in the title, for the same reason the clinic has a pet called
        // `Rex, Jr.` — `digest`'s CSV has to quote it or the row loses a column.
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

/// A ceiling on a body read into memory, not a policy: past this the read gives up
/// and the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a JSON
            // document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

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
            // `workflow`'s gate needs exactly this. The contract makes it update the
            // report document as well as the fsm instance, and the only other way to
            // see the document is `GET /api/reports/{id}` — which belongs to
            // `intake`, is a stub while `workflow` is judged alone, and answered
            // `not_implemented` to a gate that then blamed `workflow` for it.
            ["test", "report", id] => match records::get("reports", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // `digest.csv` and `digest` both start with "digest", and the CSV arm
            // must come first or a path-segment match on ["digest"] alone would
            // swallow it.
            ["api", "digest.csv"] | ["api", "digest"] => digest::handle(&method, &route, &body),
            // Before the `api/reports` arm: `/api/reports/{id}/transition` belongs
            // to `workflow`, and a match on ["api","reports",..] would hand it to
            // `intake` instead.
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
